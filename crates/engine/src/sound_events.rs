//! `SoundEvent` 0x4085b0: game events mapped to sfx, voice-queue entries and music (F7.4).
//!
//! [`sound_event3`]`(ctx, event, a, b, c)` takes the original's 2nd..4th arguments;
//! [`sound_event`] is the one-argument form (b = c = 0). Pool and counter state lives in
//! [`Ctx::sound`] ([`SoundPools`]) for the whole program run: nothing resets the pools
//! except event 0x43 with a = 0 (pool 0x4213d0), as in the original.
//!
//! Pools (F7.4): 25 ids + count + index, all counts and indices 0 in the exe. A draw
//! (`0x40b1b0`) computes the count on first use, advances the index and, when it reaches
//! the count, shuffles the items with `ShuffleSwap1000` on the shared [`Ctx::rng`] (2000
//! calls, only if count > 1) and restarts at 0. So the first draw of every pool shuffles.
//!
//! Output primitives (F7.4 helpers): `sfx` = `Play` on [`Channel::Sfx`]; `loop` = looped
//! sfx; `music` = looped [`Channel::Music`], skipped while that id already plays; `voice` =
//! `StopVoice` + `Play` on [`Channel::Voice`] (mode 1; mode 0 also skips everything while the
//! voice is busy, mode -1 plays without `StopVoice`); `Q` = `QueueVoice` (CHAR unless marked
//! SFEX). Id 0 (an empty table entry) emits nothing. Numbers below are CHAR ids unless they
//! are 4000+ (SFEX) or marked MUSI. "name(x)" = pool 0x420320[x] (player x's name line), or
//! 0x4202b0 in 1-player games (event 1 with a = 0). Out-of-range indices pick no pool
//! (id 0) instead of reading past the arrays.
//!
//! | Event | Arguments | Sounds |
//! |---|---|---|
//! | 0x01 | a = players-1 | no sound; 1-player flag = (a = 0); clears the 0x35, 0x3b, 0x54 and 0x27 state |
//! | 0x02 | - | question music: pool 0x420240 (MUSI 13-15) remembered; StopVoice; Q SFEX4055 |
//! | 0x03, 0x05-0x08 | - | music MUSI 10 / 12 / 19 / 20 / 11 |
//! | 0x04 | - | music: the question music again |
//! | 0x09 | a = volume | volume of the last music of events 2-8, if it plays |
//! | 0x0a, 0x0b | - | sfx pool 0x421eb8 / 0x421f28 |
//! | 0x0c | a = beaten millionaire 0..4 | if the voice is idle: StopVoice, Q SFEX pool 0x421f98[a] |
//! | 0x0d | a = beaten millionaire 0..4 | sfx 4045; if the voice is idle: StopVoice, Q SFEX pool 0x4221b8[a] |
//! | 0x0e-0x11 | - | sfx 4057 / 4050 / 4058 / 4054 |
//! | 0x12, 0x1b | - | sfx 4037 |
//! | 0x13, 0x16, 0x17, 0x19, 0x1a | - | sfx 4038 / 4043 / 4011 / 4166 / 4167 |
//! | 0x14, 0x15, 0x18 | - | sfx pool 0x4224b8 / 0x422528 / 0x422448 |
//! | 0x1c | a = seconds left 0..9 | sfx 4178-a |
//! | 0x1d | a = 0..2 | sfx 4182+a |
//! | 0x1e, 0x1f, 0x20, 0x2a | - | sfx 4180 / 4185 / 4179 / 4193 |
//! | 0x26 | a = 0 play, 1 query | a = 0: sfx 4189, sfx 4042; a = 1: returns 4189 while it plays |
//! | 0x27 | a = 0 start (b = 0..2), 1 stop | a = 0: loop 4187 / 4186 / 4188 for b = 0 / 1 / 2; a = 1: stop that loop |
//! | 0x28, 0x29 | a = 0 play, 1 query | sfx 4055 / 4048, or returns it while it plays |
//! | 0x2b | a = 0 start, else stop | loop 4194 or stop it; returns 4194 while it plays |
//! | 0x2d | a = question number, b = players-1 | a != 1: voice pool 0x420a50; a = 1: StopVoice, Q 356, Q pool 0x420908[min(b,1)], Q pool 0x4209e0 |
//! | 0x2e | a = remaining opponents + 1 | a = 2: voice pool 0x420830; a = 3: voice pool 0x42089c |
//! | 0x2f, 0x30 | - | voice pool 0x420e38 / 0x420ea8 |
//! | 0x31 | a = question voice id | rand: 90%: voice CHAR{a}q; 10%: StopVoice, Q CHAR{a}q, Q pool 0x420ac0 |
//! | 0x32 | a = slot, b = voice id, c = answer 0..3 | voice CHAR{b}{'a'+c} (`/quesonly`; no such clips) |
//! | 0x33 | a = slot 0..3 | a = 0 rolls v (F2.6); StopVoice, Q 236+v / 239+v / 242+v / 245+v; a = 3 also Q pool 0x420c10 |
//! | 0x34 | a = chosen slot | StopVoice, Q pool 0x4204d0[a], Q SFEX pool 0x422598 |
//! | 0x35 | a = chosen slot | once per game: StopVoice, Q pool 0x420680[a], Q SFEX pool 0x422598; later calls as 0x34 |
//! | 0x36 | a = chosen slot | StopVoice, Q 253+a, Q pool 0x420b30 |
//! | 0x37 | a = chosen, b = correct slot | StopVoice, Q 272+b, Q pool 0x420ba0 |
//! | 0x38 | a, b as 0x37, c = opponent player | StopVoice, Q 272+b, Q pool 0x420320[c], Q 452 |
//! | 0x39 | - | StopVoice, Q pool 0x420c80, Q pool 0x420a50 |
//! | 0x3a | a = answerer, b = players-1, c = beater | b < 2: voice pool 0x420cf0; else StopVoice, Q pool 0x420320[a], Q pool 0x420f18, Q pool 0x420320[c] |
//! | 0x3b | - | counter += 1; voice pool 0x420d60[counter > 3] |
//! | 0x3c | - | voice pool 0x420f88 |
//! | 0x3d | a = answerer | StopVoice, Q pool 0x420ff8, Q name(a), Q pool 0x421068 |
//! | 0x3e | a = answerer | StopVoice, Q name(a), Q pool 0x421068 |
//! | 0x3f | a = winner, b = winner money < 100000 | StopVoice, Q name(a), Q pool 0x4210d8[b] |
//! | 0x40 | a = question number | StopVoice, Q pool 0x4202b0, Q 430 (a > 7) / 432 (a < 4) / 431 |
//! | 0x41 | - | sfx 4181, voice pool 0x4211b0 |
//! | 0x42 | a = player count key - 1, 0..3 | voice pool 0x421220[a] |
//! | 0x43 | a = player index | a = 0 resets pool 0x4213d0; StopVoice, Q name(a), Q pool 0x4213d0 (1 player: Q 465) |
//! | 0x44 | a = millionaire | StopVoice, Q SFEX4057, Q pool 0x421440[a] |
//! | 0x45 | a = millionaire | StopVoice, Q pool 0x421660[a], Q SFEX4195 |
//! | 0x48 | - | voice pool 0x422608 |
//! | 0x49, 0x4a | - | voice 415 / 416 |
//! | 0x4b | - | StopVoice, Q SFEX4059, Q pool 0x422678 |
//! | 0x4c | - | voice (mode -1) pool 0x4226e8; returns the id while it plays |
//! | 0x4f | a = beater, b = beater is not the answerer | StopVoice, Q SFEX pool 0x4223d8, Q name(a), Q pool 0x421880[b] |
//! | 0x50 | - | voice (mode 0) pool 0x421958 |
//! | 0x51 | a = hits | music: question music; StopVoice; Q pool 0x4219c8 (a < 3) / 0x421a34 (< 15) / 0x421aa0 (< 20) / 0x421b10; Q SFEX4056 |
//! | 0x52 | a = beaten millionaire | voice pool 0x421b80[a] (a = 4 shares pool 0x421d30 with 0x54) |
//! | 0x54 | - | StopVoice, Q SFEX4191, Q pool 0x421d30, Q pool 0x421da0 (first call per game) / 0x421e10 |
//! | 0x55 | a = weapon 0..14 | StopVoice, Q SFEX4190, Q table 0x421e7c[a] (weapon 14: none) |
//! | 0x56 | a = 0..8 | voice 406+a |
//! | 0x57 | a = 1..4 | voice 300 / 301 / 302 / 497 |
//! | 0x58 | - | StopVoice, Q 297, Q SFEX pool 0x422598 |
//! | 0x59, 0x5c, 0x62, 0x63, 0x6a | - | voice 404 / 299 / 298 / 394 / 498 |
//! | 0x5a, 0x60 | a = chosen slot | StopVoice, Q 253+a, Q pool 0x420b30, Q 286 / 291 |
//! | 0x5b, 0x61 | a = chosen, b = correct slot | StopVoice, Q 272+b, Q pool 0x420ba0, Q 287 / 292 |
//! | 0x5d | a = answerer | StopVoice, Q name(a), Q 405 |
//! | 0x5e | - | voice pool 0x420c10 |
//! | 0x5f | - | StopVoice |
//! | 0x64 | - | voice pool 0x422758 |
//! | 0x65 | - | StopVoice, Q 391, Q SFEX4192 |
//! | 0x66, 0x67 | - | sfx 4167, voice pool 0x4227c8 / 0x422838 |
//! | 0x68 | a = chosen slot, b = slot asked of the 8-ball | StopVoice, Q 253+a, then Q 501 if a != b, else Q pool 0x4228a8 |
//! | 0x69 | a = chosen, b = correct, c = asked slot | StopVoice, Q 272+b, then Q pool 0x422918 if b = c, else Q 503 if a = c, else Q 502 |
//! | others | - | nothing (0x00, 0x21-0x25, 0x2c, 0x46, 0x47, 0x4d, 0x4e, 0x53, > 0x6a) |
//!
//! The return value is 0 except for the query events (0x26, 0x28, 0x29 with a = 1, 0x2b,
//! 0x4c), which return the sound number while it plays.
//!
//! Deviation: the original's queue call with "start" set drops the id while the voice
//! channel plays; every such call follows a `StopVoice` in the same event, so it never
//! triggers and is not modelled. Event 9 sets the music channel volume, which a later
//! track keeps (the original set it on the playing buffer only).

use crate::audio_types::{AudioCmd, Channel, Prefix, SoundId};
use crate::ctx::Ctx;
use crate::rng::Lcg;
use crate::sound_tables::*;

/// One 25-slot pool struct (0x6c bytes in the original).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pool {
    items: [i32; 25],
    count: i32,
    index: i32,
}

impl Pool {
    pub(crate) fn new(init: &[u16]) -> Self {
        let mut items = [0; 25];
        for (d, &s) in items.iter_mut().zip(init) {
            *d = i32::from(s);
        }
        Self {
            items,
            count: 0,
            index: 0,
        }
    }

    /// The pool draw of 0x40b1b0 (and its inlined copies in 0x4085b0).
    pub(crate) fn draw(&mut self, rng: &mut Lcg) -> u16 {
        if self.count == 0 {
            self.count = 25;
            while self.count > 0 && self.items[self.count as usize - 1] == 0 {
                self.count -= 1;
            }
            self.index = self.count;
            if self.count == 0 {
                return 0;
            }
        }
        self.index += 1;
        if self.count <= self.index {
            if self.count > 1 {
                rng.shuffle_swap1000(&mut self.items[..self.count as usize]);
            }
            self.index = 0;
        }
        self.items[self.index as usize] as u16
    }

    fn reset(&mut self) {
        self.count = 0;
        self.index = 0;
    }
}

/// `SoundEvent` state: every pool plus the event globals 0x425e74..0x425e90.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundPools {
    pools: Vec<Pool>,
    /// 0x425e78: question music drawn by event 2 (replayed by 4 and 0x51).
    question_music: u16,
    /// 0x425e88: music whose volume event 9 changes.
    volume_music: u16,
    /// 0x425e8c: letter voice variant rolled by 0x33 slot A.
    letter_variant: u16,
    /// 0x425e84: 1-player game (name lines use the generic pool).
    one_player: bool,
    /// 0x425e90: the low-money line of 0x35 was played this game.
    low_money_done: bool,
    /// 0x425e80: event 0x3b calls this game.
    count_3b: i32,
    /// 0x425e74: event 0x54 calls this game.
    count_54: i32,
    /// 0x425e7c: sfx loop started by event 0x27.
    crowd_loop: u16,
}

impl Default for SoundPools {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPools {
    /// State at program start: every pool in exe order, unshuffled.
    pub fn new() -> Self {
        Self {
            pools: POOL_ITEMS.iter().map(|items| Pool::new(items)).collect(),
            question_music: 0,
            volume_music: 0,
            letter_variant: 0,
            one_player: false,
            low_money_done: false,
            count_3b: 0,
            count_54: 0,
            crowd_loop: 0,
        }
    }

    /// Current order of pool `pool` (a `sound_tables::P_*` index).
    pub fn items(&self, pool: usize) -> &[i32] {
        &self.pools[pool].items[..POOL_ITEMS[pool].len()]
    }

    /// Draws the next id of `pool`; `None` (an out-of-range index) gives 0.
    pub fn draw(&mut self, rng: &mut Lcg, pool: Option<usize>) -> u16 {
        pool.map_or(0, |p| self.pools[p].draw(rng))
    }

    fn name_pool(&self, player: i32) -> Option<usize> {
        if self.one_player {
            Some(P_4202B0)
        } else {
            at(&PA_420320, player)
        }
    }
}

fn at(arr: &[usize], i: i32) -> Option<usize> {
    usize::try_from(i).ok().and_then(|i| arr.get(i).copied())
}

fn sfex(n: u16) -> SoundId {
    SoundId::new(Prefix::Sfex, n)
}

fn chr(n: u16) -> SoundId {
    SoundId::new(Prefix::Char, n)
}

/// `base + d` as a sound number; out of range gives 0 (nothing plays).
fn num(base: i32, d: i32) -> u16 {
    u16::try_from(i64::from(base) + i64::from(d)).unwrap_or(0)
}

fn draw(ctx: &mut Ctx, pool: Option<usize>) -> u16 {
    ctx.sound.draw(&mut ctx.rng, pool)
}

fn sfx(ctx: &mut Ctx, n: u16) {
    if n != 0 {
        ctx.audio(AudioCmd::Play {
            id: sfex(n),
            channel: Channel::Sfx,
            looped: false,
        });
    }
}

fn sfx_loop(ctx: &mut Ctx, n: u16) {
    ctx.audio(AudioCmd::Play {
        id: sfex(n),
        channel: Channel::Sfx,
        looped: true,
    });
}

/// 0x40bbc0: start a looping track unless that track already plays.
fn music(ctx: &mut Ctx, n: u16) {
    let id = SoundId::new(Prefix::Musi, n);
    if n != 0 && !ctx.sound_playing(id) {
        ctx.audio(AudioCmd::Play {
            id,
            channel: Channel::Music,
            looped: true,
        });
    }
}

fn stop_voice(ctx: &mut Ctx) {
    ctx.audio(AudioCmd::StopVoice);
}

/// 0x40ba50 mode 1: StopVoice, then play on the voice channel.
fn voice_id(ctx: &mut Ctx, id: SoundId) {
    stop_voice(ctx);
    if id.number != 0 {
        ctx.audio(AudioCmd::Play {
            id,
            channel: Channel::Voice,
            looped: false,
        });
    }
}

fn voice(ctx: &mut Ctx, n: u16) {
    voice_id(ctx, chr(n));
}

/// 0x40bab0 / 0x40bb40: append to the voice queue.
fn queue_id(ctx: &mut Ctx, id: SoundId) {
    if id.number != 0 {
        ctx.audio(AudioCmd::QueueVoice(id));
    }
}

fn queue(ctx: &mut Ctx, n: u16) {
    queue_id(ctx, chr(n));
}

fn queue_sfx(ctx: &mut Ctx, n: u16) {
    queue_id(ctx, sfex(n));
}

/// 0x40b700 as the original's events return it: the number while it plays, else 0.
fn query(ctx: &Ctx, id: SoundId) -> u32 {
    if ctx.sound_playing(id) {
        u32::from(id.number)
    } else {
        0
    }
}

/// Draw from a pool and play it with `voice` mode 1.
fn voice_pool(ctx: &mut Ctx, pool: Option<usize>) {
    let n = draw(ctx, pool);
    voice(ctx, n);
}

fn queue_pool(ctx: &mut Ctx, pool: Option<usize>) {
    let n = draw(ctx, pool);
    queue(ctx, n);
}

fn queue_name(ctx: &mut Ctx, player: i32) {
    let p = ctx.sound.name_pool(player);
    queue_pool(ctx, p);
}

fn sfx_pool(ctx: &mut Ctx, pool: usize) {
    let n = draw(ctx, Some(pool));
    sfx(ctx, n);
}

/// One-argument form of [`sound_event3`] (b = c = 0).
pub fn sound_event(ctx: &mut Ctx, event: u8, a: i32) -> u32 {
    sound_event3(ctx, event, a, 0, 0)
}

/// `SoundEvent(event, a, b, c)`; see the module table for each event's arguments.
pub fn sound_event3(ctx: &mut Ctx, event: u8, a: i32, b: i32, c: i32) -> u32 {
    match event {
        0x01 => {
            let s = &mut ctx.sound;
            s.low_money_done = false;
            s.one_player = a == 0;
            s.count_54 = 0;
            s.count_3b = 0;
            s.crowd_loop = 0;
        }
        0x02 => {
            let m = draw(ctx, Some(P_420240));
            ctx.sound.question_music = m;
            ctx.sound.volume_music = m;
            music(ctx, m);
            stop_voice(ctx);
            queue_sfx(ctx, 4055);
        }
        0x03 | 0x05..=0x08 => {
            let m = match event {
                0x03 => 10,
                0x05 => 12,
                0x06 => 19,
                0x07 => 20,
                _ => 11,
            };
            ctx.sound.volume_music = m;
            music(ctx, m);
        }
        0x04 => {
            let m = ctx.sound.question_music;
            ctx.sound.volume_music = m;
            music(ctx, m);
        }
        0x09 => {
            let id = SoundId::new(Prefix::Musi, ctx.sound.volume_music);
            if id.number != 0 && ctx.sound_playing(id) {
                ctx.audio(AudioCmd::SetVolume {
                    channel: Channel::Music,
                    vol: a.clamp(0, 100) as u8,
                });
            }
        }
        0x0a => sfx_pool(ctx, P_421EB8),
        0x0b => sfx_pool(ctx, P_421F28),
        0x0c | 0x0d => {
            if event == 0x0d {
                sfx(ctx, 4045);
            }
            if !ctx.voice_busy() {
                stop_voice(ctx);
                let arr = if event == 0x0c {
                    &PA_421F98
                } else {
                    &PA_4221B8
                };
                let n = draw(ctx, at(arr, a));
                queue_sfx(ctx, n);
            }
        }
        0x0e => sfx(ctx, 4057),
        0x0f => sfx(ctx, 4050),
        0x10 => sfx(ctx, 4058),
        0x11 => sfx(ctx, 4054),
        0x12 | 0x1b => sfx(ctx, 4037),
        0x13 => sfx(ctx, 4038),
        0x14 => sfx_pool(ctx, P_4224B8),
        0x15 => sfx_pool(ctx, P_422528),
        0x16 => sfx(ctx, 4043),
        0x17 => sfx(ctx, 4011),
        0x18 => sfx_pool(ctx, P_422448),
        0x19 => sfx(ctx, 4166),
        0x1a => sfx(ctx, 4167),
        0x1c => sfx(ctx, num(4178, a.saturating_neg())),
        0x1d => sfx(ctx, num(4182, a)),
        0x1e => sfx(ctx, 4180),
        0x1f => sfx(ctx, 4185),
        0x20 => sfx(ctx, 4179),
        0x26 | 0x28 | 0x29 => {
            let n = match event {
                0x26 => 4189,
                0x28 => 4055,
                _ => 4048,
            };
            match a {
                0 => {
                    sfx(ctx, n);
                    if event == 0x26 {
                        sfx(ctx, 4042);
                    }
                }
                1 => return query(ctx, sfex(n)),
                _ => {}
            }
        }
        0x27 => match a {
            0 => {
                let n = match b {
                    0 => 4187,
                    1 => 4186,
                    2 => 4188,
                    _ => return 0,
                };
                ctx.sound.crowd_loop = n;
                sfx_loop(ctx, n);
            }
            1 => {
                let n = std::mem::take(&mut ctx.sound.crowd_loop);
                if n != 0 {
                    ctx.audio(AudioCmd::StopSound(sfex(n)));
                }
            }
            _ => {}
        },
        0x2a => sfx(ctx, 4193),
        0x2b => {
            if a == 0 {
                sfx_loop(ctx, 4194);
            } else {
                ctx.audio(AudioCmd::StopSound(sfex(4194)));
            }
            return query(ctx, sfex(4194));
        }
        0x2d => {
            if a != 1 {
                voice_pool(ctx, Some(P_420A50));
            } else {
                stop_voice(ctx);
                queue(ctx, 356);
                queue_pool(ctx, at(&PA_420908, b.min(1)));
                queue_pool(ctx, Some(P_4209E0));
            }
        }
        0x2e => match a {
            2 => voice_pool(ctx, Some(P_420830)),
            3 => voice_pool(ctx, Some(P_42089C)),
            _ => {}
        },
        0x2f => voice_pool(ctx, Some(P_420E38)),
        0x30 => voice_pool(ctx, Some(P_420EA8)),
        0x31 => {
            let q = SoundId::with_suffix(Prefix::Char, a as u16, 'q');
            if ctx.rng.next() % 100 + 1 > 10 {
                voice_id(ctx, q);
            } else {
                stop_voice(ctx);
                queue_id(ctx, q);
                queue_pool(ctx, Some(P_420AC0));
            }
        }
        0x32 => {
            let letter = char::from_u32(0x61 + c as u32).unwrap_or('a');
            voice_id(ctx, SoundId::with_suffix(Prefix::Char, b as u16, letter));
        }
        0x33 => {
            if a == 0 {
                ctx.sound.letter_variant = if ctx.rng.next() % 100 + 1 < 6 {
                    2
                } else {
                    u16::from(ctx.rng.next() % 100 + 1 < 6)
                };
            }
            let base = match a {
                0 => 236,
                1 => 239,
                2 => 242,
                3 => 245,
                _ => return 0,
            };
            stop_voice(ctx);
            queue(ctx, base + ctx.sound.letter_variant);
            if a == 3 {
                queue_pool(ctx, Some(P_420C10));
            }
        }
        0x34 | 0x35 => {
            let arr = if event == 0x35 && !ctx.sound.low_money_done {
                ctx.sound.low_money_done = true;
                &PA_420680
            } else {
                &PA_4204D0
            };
            stop_voice(ctx);
            queue_pool(ctx, at(arr, a));
            let n = draw(ctx, Some(P_422598));
            queue_sfx(ctx, n);
        }
        0x36 | 0x5a | 0x60 => {
            stop_voice(ctx);
            queue(ctx, num(253, a));
            queue_pool(ctx, Some(P_420B30));
            match event {
                0x5a => queue(ctx, 286),
                0x60 => queue(ctx, 291),
                _ => {}
            }
        }
        0x37 | 0x5b | 0x61 => {
            stop_voice(ctx);
            queue(ctx, num(272, b));
            queue_pool(ctx, Some(P_420BA0));
            match event {
                0x5b => queue(ctx, 287),
                0x61 => queue(ctx, 292),
                _ => {}
            }
        }
        0x38 => {
            stop_voice(ctx);
            queue(ctx, num(272, b));
            queue_pool(ctx, at(&PA_420320, c));
            queue(ctx, 452);
        }
        0x39 => {
            stop_voice(ctx);
            queue_pool(ctx, Some(P_420C80));
            queue_pool(ctx, Some(P_420A50));
        }
        0x3a => {
            if b < 2 {
                voice_pool(ctx, Some(P_420CF0));
            } else {
                stop_voice(ctx);
                queue_pool(ctx, at(&PA_420320, a));
                queue_pool(ctx, Some(P_420F18));
                queue_pool(ctx, at(&PA_420320, c));
            }
        }
        0x3b => {
            ctx.sound.count_3b += 1;
            let k = i32::from(ctx.sound.count_3b > 3);
            voice_pool(ctx, at(&PA_420D60, k));
        }
        0x3c => voice_pool(ctx, Some(P_420F88)),
        0x3d | 0x3e => {
            stop_voice(ctx);
            if event == 0x3d {
                queue_pool(ctx, Some(P_420FF8));
            }
            queue_name(ctx, a);
            queue_pool(ctx, Some(P_421068));
        }
        0x3f => {
            stop_voice(ctx);
            queue_name(ctx, a);
            queue_pool(ctx, at(&PA_4210D8, b));
        }
        0x40 => {
            stop_voice(ctx);
            queue_pool(ctx, Some(P_4202B0));
            queue(
                ctx,
                if a > 7 {
                    430
                } else if a < 4 {
                    432
                } else {
                    431
                },
            );
        }
        0x41 => {
            sfx(ctx, 4181);
            voice_pool(ctx, Some(P_4211B0));
        }
        0x42 => voice_pool(ctx, at(&PA_421220, a)),
        0x43 => {
            if a == 0 {
                ctx.sound.pools[P_4213D0].reset();
            }
            stop_voice(ctx);
            queue_name(ctx, a);
            if ctx.sound.one_player {
                queue(ctx, 465);
            } else {
                queue_pool(ctx, Some(P_4213D0));
            }
        }
        0x44 => {
            stop_voice(ctx);
            queue_sfx(ctx, 4057);
            queue_pool(ctx, at(&PA_421440, a));
        }
        0x45 => {
            stop_voice(ctx);
            queue_pool(ctx, at(&PA_421660, a));
            queue_sfx(ctx, 4195);
        }
        0x48 => voice_pool(ctx, Some(P_422608)),
        0x49 => voice(ctx, 415),
        0x4a => voice(ctx, 416),
        0x4b => {
            stop_voice(ctx);
            queue_sfx(ctx, 4059);
            queue_pool(ctx, Some(P_422678));
        }
        0x4c => {
            let n = draw(ctx, Some(P_4226E8));
            if n != 0 {
                ctx.audio(AudioCmd::Play {
                    id: chr(n),
                    channel: Channel::Voice,
                    looped: false,
                });
                return query(ctx, chr(n));
            }
        }
        0x4f => {
            stop_voice(ctx);
            let n = draw(ctx, Some(P_4223D8));
            queue_sfx(ctx, n);
            queue_name(ctx, a);
            queue_pool(ctx, at(&PA_421880, b));
        }
        0x50 => {
            let n = draw(ctx, Some(P_421958));
            if !ctx.voice_busy() {
                voice(ctx, n);
            }
        }
        0x51 => {
            let m = ctx.sound.question_music;
            music(ctx, m);
            stop_voice(ctx);
            let p = if a < 3 {
                P_4219C8
            } else if a < 15 {
                P_421A34
            } else if a <= 19 {
                P_421AA0
            } else {
                P_421B10
            };
            queue_pool(ctx, Some(p));
            queue_sfx(ctx, 4056);
        }
        0x52 => voice_pool(ctx, at(&PA_421B80, a)),
        0x54 => {
            stop_voice(ctx);
            queue_sfx(ctx, 4191);
            queue_pool(ctx, Some(P_421D30));
            ctx.sound.count_54 += 1;
            let p = if ctx.sound.count_54 == 1 {
                P_421DA0
            } else {
                P_421E10
            };
            queue_pool(ctx, Some(p));
        }
        0x55 => {
            stop_voice(ctx);
            queue_sfx(ctx, 4190);
            let n = usize::try_from(a)
                .ok()
                .and_then(|i| WEAPON_VOICE.get(i).copied())
                .unwrap_or(0);
            queue(ctx, n);
        }
        0x56 => voice(ctx, num(406, a)),
        0x57 => match a {
            1 => voice(ctx, 300),
            2 => voice(ctx, 301),
            3 => voice(ctx, 302),
            4 => voice(ctx, 497),
            _ => {}
        },
        0x58 => {
            stop_voice(ctx);
            queue(ctx, 297);
            let n = draw(ctx, Some(P_422598));
            queue_sfx(ctx, n);
        }
        0x59 => voice(ctx, 404),
        0x5c => voice(ctx, 299),
        0x5d => {
            stop_voice(ctx);
            queue_name(ctx, a);
            queue(ctx, 405);
        }
        0x5e => voice_pool(ctx, Some(P_420C10)),
        0x5f => stop_voice(ctx),
        0x62 => voice(ctx, 298),
        0x63 => voice(ctx, 394),
        0x64 => voice_pool(ctx, Some(P_422758)),
        0x65 => {
            stop_voice(ctx);
            queue(ctx, 391);
            queue_sfx(ctx, 4192);
        }
        0x66 | 0x67 => {
            sfx(ctx, 4167);
            voice_pool(ctx, Some(if event == 0x66 { P_4227C8 } else { P_422838 }));
        }
        0x68 => {
            stop_voice(ctx);
            queue(ctx, num(253, a));
            if a != b {
                queue(ctx, 501);
            } else {
                queue_pool(ctx, Some(P_4228A8));
            }
        }
        0x69 => {
            stop_voice(ctx);
            queue(ctx, num(272, b));
            if b != c {
                queue(ctx, if a == c { 503 } else { 502 });
            } else {
                queue_pool(ctx, Some(P_422918));
            }
        }
        0x6a => voice(ctx, 498),
        _ => {}
    }
    0
}

#[cfg(test)]
mod tests;
