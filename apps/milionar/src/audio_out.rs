//! SDL audio stream fed by the shared [`engine::Mixer`].
//!
//! The stream callback runs on SDL's audio thread and only locks the mixer for one
//! `render` call. If no device opens (VM, no output), [`open`] returns `None` and the
//! game host advances the mixer by wall-clock time instead (Review Focus 5).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use engine::Mixer;
use sdl3::AudioSubsystem;
use sdl3::audio::{AudioCallback, AudioFormat, AudioSpec, AudioStream, AudioStreamWithCallback};

use crate::host::lock;

pub const RATE: u32 = 44_100;
pub const CHANNELS: u16 = 2;

/// Counters the callback keeps (diagnostics: is sound actually flowing?).
#[derive(Default)]
pub struct AudioStats {
    /// Sample frames rendered so far.
    pub frames: AtomicU64,
    /// Peak absolute sample since the last [`AudioStats::take_peak`], as f32 bits.
    peak: AtomicU32,
}

impl AudioStats {
    #[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

pub struct Feed {
    mixer: Arc<Mutex<Mixer>>,
    buf: Vec<f32>,
    stats: Arc<AudioStats>,
}

impl AudioCallback<f32> for Feed {
    fn callback(&mut self, stream: &mut AudioStream, requested: i32) {
        let ch = usize::from(CHANNELS);
        let n = (requested.max(0) as usize).div_ceil(ch) * ch;
        if n == 0 {
            return;
        }
        if self.buf.len() < n {
            self.buf.resize(n, 0.0);
        }
        let out = &mut self.buf[..n];
        lock(&self.mixer).render(out, RATE, CHANNELS);
        let _ = stream.put_data_f32(out);
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        self.stats
            .frames
            .fetch_add((n / ch) as u64, Ordering::Relaxed);
        self.stats.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }
}

/// The open audio stream. Dropping it pauses the device first.
///
/// sdl3 0.22.1's `Drop for AudioStreamWithCallback` frees the callback box (our [`Feed`])
/// before its `base_stream` field runs `SDL_DestroyAudioStream`, so SDL's audio thread
/// could enter the callback on freed memory in between. `pause()`
/// (`SDL_PauseAudioStreamDevice`) takes the device lock the callback runs under and stops
/// further callbacks, which closes that window. Every drop path (normal exit, early `?`
/// returns, a failed `resume`) goes through this guard.
pub struct AudioOut {
    stream: AudioStreamWithCallback<Feed>,
}

impl AudioOut {
    /// Pauses or resumes the device (the options overlay): no callbacks run while
    /// paused, so every channel stops where it is and resumes from there.
    pub fn set_paused(&self, on: bool) {
        let r = if on {
            self.stream.pause()
        } else {
            self.stream.resume()
        };
        if let Err(e) = r {
            eprintln!("warning: cannot pause or resume audio: {e}");
        }
    }

    /// Master volume 0..=100 (the stream gain).
    pub fn set_volume(&self, volume: u32) {
        let _ = self.stream.set_gain(volume.min(100) as f32 / 100.0);
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        // Runs before the field (and so before the callback box) is dropped.
        let _ = self.stream.pause();
    }
}

/// Opens the default playback device; `volume` is the master volume 0..=100.
pub fn open(
    audio: &AudioSubsystem,
    mixer: Arc<Mutex<Mixer>>,
    volume: u32,
    stats: Arc<AudioStats>,
) -> Option<AudioOut> {
    let spec = AudioSpec::new(
        Some(RATE as i32),
        Some(i32::from(CHANNELS)),
        Some(AudioFormat::f32_sys()),
    );
    let feed = Feed {
        mixer,
        buf: vec![0.0; 8192],
        stats,
    };
    let out = match audio.open_playback_stream(&spec, feed) {
        Ok(stream) => AudioOut { stream },
        Err(e) => {
            eprintln!("warning: no audio device ({e}); playing silently");
            return None;
        }
    };
    let _ = out.stream.set_gain(volume.min(100) as f32 / 100.0);
    if let Err(e) = out.stream.resume() {
        eprintln!("warning: cannot start audio ({e}); playing silently");
        return None;
    }
    Some(out)
}
