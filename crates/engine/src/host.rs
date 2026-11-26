//! The host interface: what the engine needs from a window/audio/input backend.

use std::fmt;

use render::Frame;

use crate::audio_types::{AudioCmd, SoundId};
use crate::ctx::DebugState;
use crate::input::InputState;

/// One game tick in milliseconds (F1.3).
pub const TICK_MS: u32 = 66;

/// How a present is paced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pacing {
    /// Ordinary screen: 66 ms since the previous present.
    Tick66,
    /// Fullscreen cutscene frame. The host ends this present `min_ms` (never under 66 ms)
    /// after the previous present's deadline, not after this call: the schedule is absolute
    /// (a late frame restarts it from now). The caller passes the time left until the
    /// frame's target (`target(n) - now_ms`, target(n) = start + n * frame time) so rounding
    /// does not drift.
    Cutscene { min_ms: u32 },
}

impl Pacing {
    /// Minimum time this present occupies.
    pub fn duration_ms(self) -> u32 {
        match self {
            Pacing::Tick66 => TICK_MS,
            Pacing::Cutscene { min_ms } => min_ms.max(TICK_MS),
        }
    }
}

/// The user closed the window; every engine loop unwinds with `?`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Quit;

impl fmt::Display for Quit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("quit requested")
    }
}

impl std::error::Error for Quit {}

/// A backend that shows frames, plays audio and delivers input.
///
/// `voice_busy` and `voice_envelope` must reflect every `audio()` command issued before
/// the call: a threaded host applies commands synchronously (e.g. to a shared mixer
/// behind a mutex), never through a flag updated later by another thread, or a loop that
/// queues a clip and then waits on `voice_busy` would fall straight through.
pub trait Host {
    /// Show `frame`, wait out the pacing, and return the input of the elapsed tick.
    /// Returns `Err(Quit)` once the user has closed the window, and on every call after.
    ///
    /// `DrawOp::Smk` references are valid only for the duration of this call (the engine
    /// may release an asset or replace a cutscene stream frame right after it returns); a
    /// threaded host must resolve or snapshot the referenced frame data inside `present`
    /// before returning.
    fn present(&mut self, frame: Frame, pacing: Pacing) -> Result<InputState, Quit>;
    fn audio(&mut self, cmd: AudioCmd);
    /// `IsVoiceBusy`: a voice clip is playing or queued.
    fn voice_busy(&self) -> bool;
    /// Lip-sync envelope byte at the voice channel's play cursor (F7.5); 0 when silent.
    fn voice_envelope(&self) -> u8;
    /// `IsSoundPlaying` 0x40b700: a clip with this id is sounding (same synchronicity rule
    /// as `voice_busy`).
    fn sound_playing(&self, id: SoundId) -> bool;
    /// `FlushInput` 0x40fb00: drop key/mouse events accumulated since the last present.
    fn flush_input(&mut self);
    /// `VoiceQueuePump` 0x40b4a0: start the next queued voice clip if none is playing.
    /// Only the engine calls it (via [`crate::Ctx::pump_voice`]), at the original's call
    /// sites; a host must not pump the voice queue on its own (e.g. in `present`).
    fn pump_voice(&mut self);
    /// Monotonic milliseconds.
    fn now_ms(&self) -> u64;
    /// Called right before every [`Host::present`] with the engine's debug state (tests and
    /// replays read e.g. the correct answer slot from it). Default: ignored.
    fn observe_debug(&mut self, _state: &DebugState) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutscene_pacing_never_below_a_tick() {
        assert_eq!(Pacing::Tick66.duration_ms(), 66);
        assert_eq!(Pacing::Cutscene { min_ms: 83 }.duration_ms(), 83);
        assert_eq!(Pacing::Cutscene { min_ms: 10 }.duration_ms(), 66);
    }
}
