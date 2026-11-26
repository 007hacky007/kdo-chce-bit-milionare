//! Host-independent audio mixer (F7.3, F7.5).
//!
//! One [`Mixer`] models the original's three channels: sfx (overlapping, at most 19 at
//! once, the oldest stopped first), voice (a queue of up to 10 clips, one playing) and
//! music (one looping track). A threaded host keeps it behind a mutex: commands are applied
//! synchronously by the game thread, the device callback calls [`Mixer::render`], and a
//! host without a device calls [`Mixer::advance_ms`] so voice timing still progresses.
//! Sources are resampled linearly on the fly; `render` allocates nothing and does not free
//! (finished clips are parked on a retired list that the next `apply` drains).
//!
//! The voice queue advances only in [`Mixer::pump_voice`], which the engine triggers
//! through `Host::pump_voice` exactly where the original calls `VoiceQueuePump` 0x40b4a0
//! (once per tick in most screen loops; not during cutscenes or the pause slide-in).
//! `render` just plays the current clip, so a clip that ends between pumps leaves silence
//! until the next pump starts the next one.
//!
//! Voice pause (0x40b910 / 0x40b970): pausing the voice channel suspends the clip that
//! is playing and freezes the queue; a clip played on the voice channel while paused (the
//! pause menu's idle line, event 0x4c) sounds normally. Resuming brings the suspended clip
//! back where it stopped once the voice slot is free.
//!
//! Threading contract: decoding a WAV and computing its envelope can take long, so a
//! threaded host must not do it under the mixer lock. It resolves the sound first with a
//! [`ClipCache`] on the game thread (`ClipCache::get`), then locks briefly and calls
//! [`Mixer::apply_clip`] with the prepared clip. [`Mixer::apply`] does both in one call and
//! is meant for single-threaded hosts (headless, tests).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use formats::wav::Pcm;

use crate::assets::Assets;
use crate::audio_types::{AudioCmd, Channel, SoundId};

const MAX_RETIRED: usize = 32;

/// Concurrent sfx limit: 20 DirectSound slots, slot 0 unused (F7.3).
pub const MAX_SFX: usize = 19;
/// Voice queue capacity (F7.3).
pub const VOICE_QUEUE_LEN: usize = 10;
/// Rate the mixer runs at when advanced without a device.
const SILENT_RATE: u32 = 22_050;

/// Where the mixer finds decoded sounds.
pub trait SoundSource {
    /// `None` when the sound does not exist; the mixer then treats it as silence.
    fn sound(&self, id: SoundId) -> Option<Arc<Pcm>>;
}

impl SoundSource for Assets {
    fn sound(&self, id: SoundId) -> Option<Arc<Pcm>> {
        Assets::sound(self, id).ok()
    }
}

impl<F: Fn(SoundId) -> Option<Arc<Pcm>>> SoundSource for F {
    fn sound(&self, id: SoundId) -> Option<Arc<Pcm>> {
        self(id)
    }
}

/// A source with no sounds: every clip is silent and zero-length.
pub struct SilentSource;

impl SoundSource for SilentSource {
    fn sound(&self, _id: SoundId) -> Option<Arc<Pcm>> {
        None
    }
}

/// Linear gain for the original volume scale: DSBVOLUME = (vol*5 - 500) * 4 hundredths of
/// a dB, clamped to [-2000, 0]; gain = 10^(dsb / 2000) (vol 100 = 1.0).
pub fn volume_gain(vol: u8) -> f32 {
    let dsb = ((i32::from(vol) * 5 - 500) * 4).clamp(-2000, 0);
    10f32.powf(dsb as f32 / 2000.0)
}

/// F7.5 envelope, exactly as 0x40ce30 computes it over the raw data bytes: a buffer of
/// `(len >> 11) + 1` bytes; for each raw byte `i` (8-bit data skips 0x80 bytes, 16-bit
/// data adds raw bytes, not samples) the byte is added to a running sum and, when
/// `i & 0x7ff == 0`, `out[i >> 11] = sum >> 11` and the sum restarts. So block 0 is always
/// 0, block k >= 1 holds the sum of bytes `2048(k-1)+1 ..= 2048k` divided by 2048, and a
/// trailing partial block stays 0. The raw bytes are rebuilt from the widened samples.
pub fn compute_envelope(pcm: &Pcm) -> Vec<u8> {
    let eight = pcm.bits_per_sample == 8;
    let len = if eight {
        pcm.samples.len()
    } else {
        pcm.samples.len() * 2
    };
    let mut out = vec![0u8; (len >> 11) + 1];
    let mut sum: u32 = 0;
    let mut i = 0usize;
    let mut step = |b: u8, out: &mut Vec<u8>| {
        if !(eight && b == 0x80) {
            sum += u32::from(b);
        }
        if i & 0x7ff == 0 {
            out[i >> 11] = (sum >> 11) as u8;
            sum = 0;
        }
        i += 1;
    };
    if eight {
        for &s in &pcm.samples {
            step(((s >> 8) + 128) as u8, &mut out);
        }
    } else {
        for &s in &pcm.samples {
            let [lo, hi] = s.to_le_bytes();
            step(lo, &mut out);
            step(hi, &mut out);
        }
    }
    out
}

/// A decoded sound with its lip-sync envelope, ready to hand to the mixer.
pub struct Clip {
    pcm: Arc<Pcm>,
    envelope: Vec<u8>,
}

impl Clip {
    fn frames(&self) -> usize {
        self.pcm.samples.len() / usize::from(self.pcm.channels.max(1))
    }

    fn silent() -> Arc<Clip> {
        Arc::new(Clip {
            pcm: Arc::new(Pcm {
                sample_rate: SILENT_RATE,
                channels: 1,
                bits_per_sample: 16,
                samples: Vec::new(),
            }),
            envelope: Vec::new(),
        })
    }
}

struct Playing {
    id: SoundId,
    clip: Arc<Clip>,
    /// Play cursor in source frames.
    pos: f64,
    looped: bool,
}

impl Playing {
    fn new(id: SoundId, clip: Arc<Clip>, looped: bool) -> Self {
        Self {
            id,
            clip,
            pos: 0.0,
            looped,
        }
    }

    fn envelope(&self) -> u8 {
        let c = &self.clip;
        let bytes_per_sample = if c.pcm.bits_per_sample == 8 { 1 } else { 2 };
        let byte = self.pos as usize * usize::from(c.pcm.channels) * bytes_per_sample;
        c.envelope.get(byte / 2048).copied().unwrap_or(0)
    }

    /// Mix into interleaved `out`; returns whether the clip has ended and how many frames
    /// it wrote.
    fn mix(&mut self, out: &mut [f32], rate: u32, ch: usize, gain: f32) -> (bool, usize) {
        let pcm = &self.clip.pcm;
        let frames = self.clip.frames();
        if frames == 0 {
            return (true, 0);
        }
        let src_ch = usize::from(pcm.channels.max(1));
        let step = f64::from(pcm.sample_rate) / f64::from(rate.max(1));
        let scale = gain / 32768.0;
        for (n, frame) in out.chunks_exact_mut(ch).enumerate() {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let j = if i + 1 < frames {
                i + 1
            } else if self.looped {
                0
            } else {
                i
            };
            let at = |f: usize, c: usize| f32::from(pcm.samples[f * src_ch + c.min(src_ch - 1)]);
            let lerp = |c: usize| at(i, c) + (at(j, c) - at(i, c)) * frac;
            if ch == 1 {
                let v = if src_ch == 1 {
                    lerp(0)
                } else {
                    (lerp(0) + lerp(1)) * 0.5
                };
                frame[0] += v * scale;
            } else {
                for (c, o) in frame.iter_mut().enumerate() {
                    *o += lerp(c) * scale;
                }
            }
            self.pos += step;
            if self.pos >= frames as f64 {
                if self.looped {
                    self.pos -= frames as f64;
                } else {
                    return (true, n + 1);
                }
            }
        }
        (false, out.len() / ch)
    }
}

struct StreamChunk {
    pcm: Pcm,
}

/// Decodes sounds and computes their envelopes outside the mixer lock; clips are cached.
pub struct ClipCache {
    source: Box<dyn SoundSource + Send>,
    clips: HashMap<SoundId, Arc<Clip>>,
}

impl ClipCache {
    pub fn new(source: Box<dyn SoundSource + Send>) -> Self {
        Self {
            source,
            clips: HashMap::new(),
        }
    }

    /// The clip for `id`, decoding it on first use; `None` if the sound does not exist.
    pub fn get(&mut self, id: SoundId) -> Option<Arc<Clip>> {
        Self::resolve(&mut self.clips, id, &*self.source)
    }

    fn resolve(
        clips: &mut HashMap<SoundId, Arc<Clip>>,
        id: SoundId,
        source: &dyn SoundSource,
    ) -> Option<Arc<Clip>> {
        if let Some(c) = clips.get(&id) {
            return Some(c.clone());
        }
        let pcm = source.sound(id)?;
        let envelope = compute_envelope(&pcm);
        let clip = Arc::new(Clip { pcm, envelope });
        clips.insert(id, clip.clone());
        Some(clip)
    }
}

/// The sound an `apply_clip` call needs resolved, if any.
pub fn command_sound(cmd: &AudioCmd) -> Option<SoundId> {
    match cmd {
        AudioCmd::Play { id, .. } | AudioCmd::QueueVoice(id) => Some(*id),
        // StopSound needs no clip.
        _ => None,
    }
}

/// The three-channel mixer.
pub struct Mixer {
    sfx: Vec<Playing>,
    voice: Option<Playing>,
    /// Clip suspended by `PauseChannel(Voice, true)`, resumed by `PauseChannel(.., false)`.
    suspended: Option<Playing>,
    queue: VecDeque<(SoundId, Arc<Clip>)>,
    music: Option<Playing>,
    stream: VecDeque<StreamChunk>,
    stream_pos: f64,
    volume: [u8; 3],
    paused: [bool; 3],
    clips: HashMap<SoundId, Arc<Clip>>,
    retired_clips: Vec<Arc<Clip>>,
    retired_chunks: Vec<StreamChunk>,
    /// Silent-mode clock: milliseconds advanced and frames already produced.
    clock_ms: u64,
    clock_frames: u64,
    scratch: Vec<f32>,
}

fn idx(c: Channel) -> usize {
    match c {
        Channel::Sfx => 0,
        Channel::Voice => 1,
        Channel::Music => 2,
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            sfx: Vec::with_capacity(MAX_SFX + 1),
            voice: None,
            suspended: None,
            queue: VecDeque::with_capacity(VOICE_QUEUE_LEN),
            music: None,
            stream: VecDeque::new(),
            stream_pos: 0.0,
            volume: [100; 3],
            paused: [false; 3],
            clips: HashMap::new(),
            retired_clips: Vec::with_capacity(MAX_RETIRED),
            retired_chunks: Vec::with_capacity(MAX_RETIRED),
            clock_ms: 0,
            clock_frames: 0,
            scratch: Vec::new(),
        }
    }

    /// Resolves the sound and applies the command in one call. Decodes under whatever lock
    /// the caller holds; threaded hosts use [`ClipCache`] and [`Mixer::apply_clip`] instead.
    pub fn apply(&mut self, cmd: AudioCmd, source: &dyn SoundSource) {
        let clip =
            command_sound(&cmd).and_then(|id| ClipCache::resolve(&mut self.clips, id, source));
        self.apply_clip(cmd, clip);
    }

    /// Applies a command with its sound already resolved (`clip` is ignored by commands
    /// without a sound). Cheap: no decoding. Takes effect immediately, so `voice_busy`
    /// and `voice_envelope` reflect it at once.
    pub fn apply_clip(&mut self, cmd: AudioCmd, clip: Option<Arc<Clip>>) {
        self.retired_clips.clear();
        self.retired_chunks.clear();
        match cmd {
            AudioCmd::Play {
                id,
                channel,
                looped,
            } => match channel {
                Channel::Sfx => {
                    if let Some(c) = clip {
                        if self.sfx.len() >= MAX_SFX {
                            self.sfx.remove(0);
                        }
                        self.sfx.push(Playing::new(id, c, looped));
                    }
                }
                Channel::Voice => {
                    let c = clip.unwrap_or_else(Clip::silent);
                    self.voice = Some(Playing::new(id, c, looped));
                }
                Channel::Music => {
                    self.music = clip.map(|c| Playing::new(id, c, looped));
                }
            },
            AudioCmd::QueueVoice(id) => {
                let c = clip.unwrap_or_else(Clip::silent);
                if self.voice.is_none()
                    && self.suspended.is_none()
                    && !self.paused[1]
                    && self.queue.is_empty()
                {
                    self.voice = Some(Playing::new(id, c, false));
                } else if self.queue.len() < VOICE_QUEUE_LEN {
                    self.queue.push_back((id, c));
                }
            }
            AudioCmd::StopSound(id) => {
                let mut i = 0;
                while i < self.sfx.len() {
                    if self.sfx[i].id == id {
                        let p = self.sfx.remove(i);
                        self.retire(p.clip);
                    } else {
                        i += 1;
                    }
                }
                if self.voice.as_ref().is_some_and(|p| p.id == id) {
                    self.voice = None;
                }
                if self.suspended.as_ref().is_some_and(|p| p.id == id) {
                    self.suspended = None;
                }
                if self.music.as_ref().is_some_and(|p| p.id == id) {
                    self.music = None;
                }
            }
            AudioCmd::StopVoice | AudioCmd::StopChannel(Channel::Voice) => {
                self.queue.clear();
                self.voice = None;
                self.suspended = None;
            }
            AudioCmd::StopChannel(Channel::Sfx) => self.sfx.clear(),
            AudioCmd::StopChannel(Channel::Music) => self.music = None,
            AudioCmd::SetVolume { channel, vol } => self.volume[idx(channel)] = vol,
            AudioCmd::PauseChannel(Channel::Voice, true) => {
                self.paused[1] = true;
                if self.suspended.is_none() {
                    self.suspended = self.voice.take();
                }
            }
            AudioCmd::PauseChannel(Channel::Voice, false) => {
                self.paused[1] = false;
                if self.voice.is_none() {
                    self.voice = self.suspended.take();
                }
            }
            AudioCmd::PauseChannel(channel, p) => self.paused[idx(channel)] = p,
            AudioCmd::CutsceneAudio(samples, rate, channels) => {
                if rate > 0 && (1..=2).contains(&channels) && !samples.is_empty() {
                    self.stream.push_back(StreamChunk {
                        pcm: Pcm {
                            sample_rate: rate,
                            channels,
                            bits_per_sample: 16,
                            samples,
                        },
                    });
                    // Every queued chunk must be able to retire on the audio thread
                    // without being freed there (`retired_chunks` is empty here).
                    let want = self.stream.len();
                    self.retired_chunks.reserve(want);
                }
            }
            AudioCmd::StopCutsceneAudio => {
                self.stream.clear();
                self.stream_pos = 0.0;
            }
        }
    }

    /// Mixes into `out` (interleaved f32, `device_channels` wide) at `device_rate`,
    /// overwriting it and clamping to [-1, 1]. Plays only the current voice clip; a finished
    /// clip leaves silence until the next [`Mixer::pump_voice`].
    pub fn render(&mut self, out: &mut [f32], device_rate: u32, device_channels: u16) {
        out.fill(0.0);
        let ch = usize::from(device_channels.max(1));
        if !self.paused[0] {
            let g = volume_gain(self.volume[0]);
            let mut i = 0;
            while i < self.sfx.len() {
                if self.sfx[i].mix(out, device_rate, ch, g).0 {
                    let p = self.sfx.remove(i);
                    self.retire(p.clip);
                } else {
                    i += 1;
                }
            }
        }
        // The voice slot always plays: a paused clip sits in `suspended`.
        let g = volume_gain(self.volume[1]);
        if let Some(p) = &mut self.voice
            && p.mix(out, device_rate, ch, g).0
            && let Some(p) = self.voice.take()
        {
            self.retire(p.clip);
        }
        if !self.paused[2] {
            let g = volume_gain(self.volume[2]);
            if let Some(p) = &mut self.music
                && p.mix(out, device_rate, ch, g).0
                && let Some(p) = self.music.take()
            {
                self.retire(p.clip);
            }
        }
        self.mix_stream(out, device_rate, ch);
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }

    /// `VoiceQueuePump` (0x40b4a0): if no voice clip is playing, resume a suspended one
    /// or start the next queued one. Frozen while the voice channel is paused. Called on
    /// the engine's request (`Host::pump_voice`), never per render.
    pub fn pump_voice(&mut self) {
        if self.paused[1] || self.voice.is_some() {
            return;
        }
        if let Some(p) = self.suspended.take() {
            self.voice = Some(p);
        } else if let Some((id, c)) = self.queue.pop_front() {
            self.voice = Some(Playing::new(id, c, false));
        }
    }

    /// Parks a finished clip so the render path never frees; the next `apply_clip` drops it.
    fn retire(&mut self, clip: Arc<Clip>) {
        if self.retired_clips.len() < self.retired_clips.capacity() {
            self.retired_clips.push(clip);
        }
    }

    fn mix_stream(&mut self, out: &mut [f32], rate: u32, ch: usize) {
        let mut done = 0;
        while let Some(chunk) = self.stream.front() {
            let pcm = &chunk.pcm;
            let src_ch = usize::from(pcm.channels);
            let frames = pcm.samples.len() / src_ch;
            let step = f64::from(pcm.sample_rate) / f64::from(rate.max(1));
            let mut pos = self.stream_pos;
            while done < out.len() / ch && pos < frames as f64 {
                let i = pos as usize;
                let frac = (pos - i as f64) as f32;
                let j = (i + 1).min(frames - 1);
                let at = |f: usize, c: usize| {
                    f32::from(pcm.samples[f * src_ch + c.min(src_ch - 1)]) / 32768.0
                };
                for c in 0..ch {
                    let cc = if ch == 1 { 0 } else { c };
                    let mut v = at(i, cc) + (at(j, cc) - at(i, cc)) * frac;
                    if ch == 1 && src_ch == 2 {
                        let r = at(i, 1) + (at(j, 1) - at(i, 1)) * frac;
                        v = (v + r) * 0.5;
                    }
                    out[done * ch + c] += v;
                }
                pos += step;
                done += 1;
            }
            if pos >= frames as f64 {
                if let Some(c) = self.stream.pop_front()
                    && self.retired_chunks.len() < self.retired_chunks.capacity()
                {
                    self.retired_chunks.push(c);
                }
                self.stream_pos = 0.0;
            } else {
                self.stream_pos = pos;
                break;
            }
        }
    }

    /// Advances time without a device: renders `ms` of audio into a scratch buffer so
    /// clips end and the queue pumps exactly as with a device.
    pub fn advance_ms(&mut self, ms: u32) {
        self.clock_ms += u64::from(ms);
        let total = self.clock_ms * u64::from(SILENT_RATE) / 1000;
        let frames = (total - self.clock_frames) as usize;
        self.clock_frames = total;
        let mut buf = std::mem::take(&mut self.scratch);
        buf.resize(frames, 0.0);
        self.render(&mut buf, SILENT_RATE, 1);
        self.scratch = buf;
    }

    /// A voice clip is playing or queued (`IsVoiceBusy`).
    pub fn voice_busy(&self) -> bool {
        self.voice.is_some() || self.suspended.is_some() || !self.queue.is_empty()
    }

    /// F7.5 envelope byte at the voice play cursor; 0 when silent.
    pub fn voice_envelope(&self) -> u8 {
        self.voice.as_ref().map_or(0, Playing::envelope)
    }

    /// `IsSoundPlaying` 0x40b700: a clip with this id is sounding on any channel (a
    /// missing, silent placeholder clip and a queued, not yet started clip do not count).
    pub fn is_playing(&self, id: SoundId) -> bool {
        let hit = |p: &Playing| p.id == id && p.clip.frames() > 0;
        self.sfx.iter().any(hit)
            || self.voice.as_ref().is_some_and(hit)
            || self.music.as_ref().is_some_and(hit)
    }

    /// Number of sfx currently sounding.
    pub fn sfx_count(&self) -> usize {
        self.sfx.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_types::Prefix;

    const RATE: u32 = 22_050;

    fn id(n: u16) -> SoundId {
        SoundId::new(Prefix::Char, n)
    }

    fn pcm_ms(ms: u32, value: i16) -> Arc<Pcm> {
        Arc::new(Pcm {
            sample_rate: RATE,
            channels: 1,
            bits_per_sample: 16,
            samples: vec![value; (ms * RATE / 1000) as usize],
        })
    }

    /// Clip `n` lasts `n` ms and holds a constant level of 1000 + n.
    fn source() -> impl SoundSource {
        |sid: SoundId| Some(pcm_ms(u32::from(sid.number), 1000 + sid.number as i16))
    }

    fn render_ms(m: &mut Mixer, ms: u32) -> Vec<f32> {
        let mut out = vec![0.0; (ms * RATE / 1000) as usize];
        m.render(&mut out, RATE, 1);
        out
    }

    fn near(a: f32, level: i16) -> bool {
        (a - f32::from(level) / 32768.0).abs() < 1e-4
    }

    #[test]
    fn finished_cutscene_chunks_never_drop_on_the_render_path() {
        let mut m = Mixer::new();
        let n = MAX_RETIRED * 3;
        for _ in 0..n {
            m.apply_clip(AudioCmd::CutsceneAudio(vec![100; 220], RATE, 1), None);
        }
        let mut buf = vec![0.0; 220 * n];
        m.render(&mut buf, RATE, 1);
        assert!(m.stream.is_empty());
        assert_eq!(m.retired_chunks.len(), n);
    }

    #[test]
    fn stop_sound_and_is_playing_by_id() {
        let src = source();
        let mut m = Mixer::new();
        let sfx = |n: u16, looped| AudioCmd::Play {
            id: SoundId::new(Prefix::Sfex, n),
            channel: Channel::Sfx,
            looped,
        };
        m.apply(sfx(300, true), &src);
        m.apply(sfx(400, false), &src);
        m.apply(AudioCmd::QueueVoice(id(100)), &src);
        m.apply(AudioCmd::QueueVoice(id(200)), &src);
        assert!(m.is_playing(SoundId::new(Prefix::Sfex, 300)));
        assert!(m.is_playing(id(100)));
        assert!(!m.is_playing(id(200)), "queued, not started");
        assert!(
            !m.is_playing(SoundId::new(Prefix::Char, 300)),
            "prefix matters"
        );
        m.apply(AudioCmd::StopSound(SoundId::new(Prefix::Sfex, 300)), &src);
        assert!(!m.is_playing(SoundId::new(Prefix::Sfex, 300)));
        assert!(m.is_playing(SoundId::new(Prefix::Sfex, 400)));
        assert_eq!(m.sfx_count(), 1);
        // A missing clip is a silent placeholder that does not count as playing.
        let none = |_: SoundId| None;
        m.apply(AudioCmd::StopVoice, &none);
        m.apply(AudioCmd::QueueVoice(id(7)), &none);
        assert!(m.voice_busy());
        assert!(!m.is_playing(id(7)));
    }

    #[test]
    fn voice_queue_plays_in_order() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(100)), &src);
        m.apply(AudioCmd::QueueVoice(id(200)), &src);
        assert!(m.voice_busy());
        let a = render_ms(&mut m, 100);
        assert!(a.iter().all(|&s| near(s, 1100)));
        assert!(m.voice_busy());
        // The next clip starts only at the next pump.
        let gap = render_ms(&mut m, 100);
        assert!(gap.iter().all(|&s| s == 0.0));
        assert!(m.voice_busy());
        m.pump_voice();
        let b = render_ms(&mut m, 100);
        assert!(near(b[10], 1200));
        assert!(m.voice_busy());
        render_ms(&mut m, 100);
        assert!(!m.voice_busy());
    }

    #[test]
    fn stop_voice_clears_queue() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(100)), &src);
        m.apply(AudioCmd::QueueVoice(id(100)), &src);
        render_ms(&mut m, 10);
        m.apply(AudioCmd::StopVoice, &src);
        assert!(!m.voice_busy());
        let out = render_ms(&mut m, 300);
        assert!(out.iter().all(|&s| s == 0.0));
        assert!(!m.voice_busy());
    }

    #[test]
    fn music_replaces_music() {
        let src = source();
        let mut m = Mixer::new();
        let play = |n| AudioCmd::Play {
            id: id(n),
            channel: Channel::Music,
            looped: true,
        };
        m.apply(play(100), &src);
        m.apply(play(200), &src);
        let out = render_ms(&mut m, 500);
        // Only the second track sounds, and it loops past its 200 ms length.
        assert!(out.iter().all(|&s| near(s, 1200)));
        assert!(!m.voice_busy());
    }

    #[test]
    fn volume_curve() {
        assert!((volume_gain(100) - 1.0).abs() < 1e-6);
        assert!((volume_gain(50) - 10f32.powf(-0.5)).abs() < 1e-6);
        // Clamped at -20 dB, and never above 0 dB.
        assert!((volume_gain(0) - 0.1).abs() < 1e-6);
        assert!((volume_gain(255) - 1.0).abs() < 1e-6);
        let src = source();
        let mut m = Mixer::new();
        m.apply(
            AudioCmd::SetVolume {
                channel: Channel::Voice,
                vol: 50,
            },
            &src,
        );
        m.apply(AudioCmd::QueueVoice(id(100)), &src);
        let out = render_ms(&mut m, 10);
        assert!((out[5] - 1100.0 / 32768.0 * 10f32.powf(-0.5)).abs() < 1e-5);
    }

    /// The loop of 0x40ce30 over raw data bytes.
    fn formula(raw: &[u8], bits: u16) -> Vec<u8> {
        let mut out = vec![0u8; (raw.len() >> 11) + 1];
        let mut sum = 0u32;
        for (i, &b) in raw.iter().enumerate() {
            if !(bits == 8 && b == 0x80) {
                sum += u32::from(b);
            }
            if i & 0x7ff == 0 {
                out[i >> 11] = (sum >> 11) as u8;
                sum = 0;
            }
        }
        out
    }

    #[test]
    fn envelope_is_shifted_one_byte_with_zero_tail() {
        // 5000 bytes of 0xff: block 0 = 0xff >> 11 = 0, block 1 = bytes 1..=2048 -> 255,
        // block 2 = bytes 2049..=4096 would be written at i = 4096 -> 255; the partial
        // tail (bytes 4097..5000) is never written.
        let pcm = Pcm {
            sample_rate: RATE,
            channels: 1,
            bits_per_sample: 8,
            samples: vec![127 << 8; 5000],
        };
        assert_eq!(compute_envelope(&pcm), vec![0, 255, 255]);
        // 4096 bytes: the buffer is (4096 >> 11) + 1 = 3 long and block 2 is never written.
        let pcm16 = Pcm {
            sample_rate: RATE,
            channels: 1,
            bits_per_sample: 16,
            samples: vec![-1; 2048],
        };
        assert_eq!(compute_envelope(&pcm16), vec![0, 255, 0]);
        let long = Pcm {
            samples: vec![-1; 2048 + 1024],
            ..pcm16
        };
        assert_eq!(compute_envelope(&long), vec![0, 255, 255, 0]);
    }

    #[test]
    fn queued_clip_waits_for_the_pump() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(50)), &src);
        m.apply(AudioCmd::QueueVoice(id(50)), &src);
        let out = render_ms(&mut m, 90);
        assert!(near(out[0], 1050));
        assert_eq!(out[1500], 0.0);
        assert!(m.voice_busy());
        m.pump_voice();
        assert!(near(render_ms(&mut m, 10)[0], 1050));
    }

    #[test]
    fn output_is_clamped() {
        let loud = |_: SoundId| Some(pcm_ms(100, 30000));
        let mut m = Mixer::new();
        for _ in 0..3 {
            m.apply(
                AudioCmd::Play {
                    id: id(1),
                    channel: Channel::Sfx,
                    looped: false,
                },
                &loud,
            );
        }
        let out = render_ms(&mut m, 10);
        assert!(out.iter().all(|&s| s == 1.0));
    }

    #[test]
    fn prepared_clips_apply_without_a_source() {
        let mut cache = ClipCache::new(Box::new(source()));
        let cmd = AudioCmd::QueueVoice(id(100));
        let clip = command_sound(&cmd).and_then(|i| cache.get(i));
        assert!(clip.is_some());
        let mut m = Mixer::new();
        m.apply_clip(cmd, clip);
        assert!(m.voice_busy());
        m.advance_ms(101);
        assert!(!m.voice_busy());
    }

    #[test]
    fn envelope_matches_original_formula() {
        let samples: Vec<i16> = (0..5000).map(|i| ((i * 37) % 2000 - 1000) as i16).collect();
        let raw: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let pcm = Pcm {
            sample_rate: RATE,
            channels: 1,
            bits_per_sample: 16,
            samples,
        };
        assert_eq!(compute_envelope(&pcm), formula(&raw, 16));

        let s8: Vec<i16> = (0..3000)
            .map(|i| (((i * 11) % 256) as i16 - 128) << 8)
            .collect();
        let raw8: Vec<u8> = s8.iter().map(|&s| ((s >> 8) + 128) as u8).collect();
        let pcm8 = Pcm {
            sample_rate: RATE,
            channels: 1,
            bits_per_sample: 8,
            samples: s8,
        };
        assert_eq!(compute_envelope(&pcm8), formula(&raw8, 8));

        // The mixer reports the envelope at the play cursor (2048 data bytes = 1024 frames).
        let big: Vec<i16> = (0..4096)
            .map(|i| if i < 1024 { 0x4040 } else { 0x7070 })
            .collect();
        let pcm = Arc::new(Pcm {
            sample_rate: RATE,
            channels: 1,
            bits_per_sample: 16,
            samples: big,
        });
        let src = move |_: SoundId| Some(pcm.clone());
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(1)), &src);
        assert_eq!(m.voice_envelope(), 0);
        let mut out = vec![0.0; 1024];
        m.render(&mut out, RATE, 1);
        assert_eq!(m.voice_envelope(), 0x40);
        m.render(&mut out, RATE, 1);
        assert_eq!(m.voice_envelope(), 0x70);
        m.apply(AudioCmd::StopVoice, &src);
        assert_eq!(m.voice_envelope(), 0);
    }

    #[test]
    fn silent_device_still_ends_voice() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(500)), &src);
        m.advance_ms(499);
        assert!(m.voice_busy());
        m.advance_ms(2);
        assert!(!m.voice_busy());
    }

    #[test]
    fn missing_sound_is_a_zero_length_voice_clip() {
        let none = |_: SoundId| None;
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(1)), &none);
        assert!(m.voice_busy());
        m.advance_ms(66);
        assert!(!m.voice_busy());
    }

    #[test]
    fn pause_freezes_and_sfx_cap() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(100)), &src);
        m.apply(AudioCmd::PauseChannel(Channel::Voice, true), &src);
        m.advance_ms(1000);
        assert!(m.voice_busy());
        m.apply(AudioCmd::PauseChannel(Channel::Voice, false), &src);
        m.advance_ms(101);
        assert!(!m.voice_busy());
        for n in 0..25 {
            m.apply(
                AudioCmd::Play {
                    id: id(1000 + n),
                    channel: Channel::Sfx,
                    looped: false,
                },
                &src,
            );
        }
        assert_eq!(m.sfx_count(), 19);
    }

    #[test]
    fn voice_pause_suspends_clip_and_lets_a_new_line_play() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::QueueVoice(id(300)), &src);
        m.apply(AudioCmd::QueueVoice(id(50)), &src);
        m.advance_ms(100);
        m.apply(AudioCmd::PauseChannel(Channel::Voice, true), &src);
        assert!(!m.is_playing(id(300)));
        // A line played while paused (event 0x4c, no StopVoice) sounds and ends.
        m.apply(
            AudioCmd::Play {
                id: id(120),
                channel: Channel::Voice,
                looped: false,
            },
            &src,
        );
        assert!(m.is_playing(id(120)));
        m.advance_ms(60);
        m.pump_voice();
        assert!(m.is_playing(id(120)), "queue frozen while paused");
        // P: stop the line, resume: the suspended clip continues where it stopped.
        m.apply(AudioCmd::StopSound(id(120)), &src);
        m.apply(AudioCmd::PauseChannel(Channel::Voice, false), &src);
        assert!(m.is_playing(id(300)));
        m.advance_ms(199);
        assert!(m.is_playing(id(300)));
        m.advance_ms(2);
        assert!(!m.is_playing(id(300)));
        assert!(m.voice_busy(), "queued clip still pending");
        m.pump_voice();
        assert!(m.is_playing(id(50)));
        // StopVoice also drops a suspended clip.
        m.apply(AudioCmd::PauseChannel(Channel::Voice, true), &src);
        m.apply(AudioCmd::StopVoice, &src);
        m.apply(AudioCmd::PauseChannel(Channel::Voice, false), &src);
        assert!(!m.voice_busy());
    }

    #[test]
    fn cutscene_audio_plays_in_order_and_resamples() {
        let src = source();
        let mut m = Mixer::new();
        m.apply(AudioCmd::CutsceneAudio(vec![1000; 100], RATE, 1), &src);
        m.apply(AudioCmd::CutsceneAudio(vec![2000; 100], RATE, 1), &src);
        let mut out = vec![0.0; 1000];
        m.render(&mut out, 2 * RATE, 2);
        assert!(near(out[20], 1000) && near(out[21], 1000));
        assert!(near(out[2 * 300], 2000));
        assert_eq!(out[2 * 450], 0.0);
    }
}
