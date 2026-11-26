//! A host without window or audio device, driven by a script. Used by tests and replays.
//!
//! Script ticks are present indices: events tagged `t` are delivered by the present with
//! index `t` (0-based), i.e. they appear in the input returned by the (t+1)-th present.
//! Time advances by the pacing duration of each present; no real waiting happens. Audio
//! runs through the real [`Mixer`], advanced by the same durations, so voice timing and the
//! lip-sync envelope come from the same model as the device host.

use render::Frame;

use crate::audio::{Mixer, SilentSource, SoundSource};
use crate::audio_types::{AudioCmd, SoundId};
use crate::ctx::DebugState;
use crate::host::{Host, Pacing, Quit};
use crate::input::{InputAccumulator, InputState, MouseButton, Vk};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptEvent {
    KeyDown(Vk),
    KeyUp(Vk),
    /// Down and up within the same tick.
    Tap(Vk),
    MouseMove(i32, i32),
    MouseDown(MouseButton),
    /// The window is closed.
    Quit,
}

pub struct HeadlessHost {
    script: Vec<(u64, ScriptEvent)>,
    cursor: usize,
    latch: InputAccumulator,
    mixer: Mixer,
    source: Box<dyn SoundSource + Send>,
    /// Number of presents so far (the current tick index).
    tick: u64,
    now_ms: u64,
    quit: bool,
    frame_hashes: Vec<u64>,
    pacings: Vec<Pacing>,
    audio_log: Vec<(u64, AudioCmd)>,
    last_frame: Option<Frame>,
    /// Every presented frame, kept only after [`HeadlessHost::keep_frames`].
    frames: Option<Vec<Frame>>,
    responder: Option<Responder>,
}

/// Script callback: given the present index about to happen and the engine's debug
/// state, the events that this present delivers.
pub type Responder = Box<dyn FnMut(u64, &DebugState) -> Vec<ScriptEvent> + Send>;

impl HeadlessHost {
    /// `source` supplies the sounds (e.g. `Assets`, boxed behind a reference wrapper or an
    /// owned table); clips last their real length, rounded up to whole mixer renders (one
    /// per present).
    pub fn new(mut script: Vec<(u64, ScriptEvent)>, source: Box<dyn SoundSource + Send>) -> Self {
        script.sort_by_key(|&(t, _)| t);
        Self {
            script,
            cursor: 0,
            latch: InputAccumulator::new(),
            mixer: Mixer::new(),
            source,
            tick: 0,
            now_ms: 0,
            quit: false,
            frame_hashes: Vec::new(),
            pacings: Vec::new(),
            audio_log: Vec::new(),
            last_frame: None,
            frames: None,
            responder: None,
        }
    }

    /// React to the engine's state on top of the fixed script (e.g. answer with the
    /// correct slot); the returned events are delivered by the coming present.
    pub fn set_responder(&mut self, r: Responder) {
        self.responder = Some(r);
    }

    /// A host with no sounds: every voice clip is silent and lasts one tick.
    pub fn with_script(script: Vec<(u64, ScriptEvent)>) -> Self {
        Self::new(script, Box::new(SilentSource))
    }

    /// Presents so far.
    pub fn ticks(&self) -> u64 {
        self.tick
    }

    /// `Frame::stable_hash` of every presented frame, in order.
    pub fn frame_hashes(&self) -> &[u64] {
        &self.frame_hashes
    }

    pub fn pacings(&self) -> &[Pacing] {
        &self.pacings
    }

    /// Every audio command with the tick (present count) at which it was issued.
    pub fn audio_log(&self) -> &[(u64, AudioCmd)] {
        &self.audio_log
    }

    pub fn last_frame(&self) -> Option<&Frame> {
        self.last_frame.as_ref()
    }

    /// Keep a copy of every presented frame from now on (tests asserting sequences).
    pub fn keep_frames(&mut self) {
        self.frames.get_or_insert_with(Vec::new);
    }

    /// Frames kept since [`keep_frames`](Self::keep_frames), in present order.
    pub fn frames(&self) -> &[Frame] {
        self.frames.as_deref().unwrap_or(&[])
    }
}

impl HeadlessHost {
    fn apply(&mut self, ev: ScriptEvent) {
        match ev {
            ScriptEvent::KeyDown(vk) => self.latch.key_down(vk),
            ScriptEvent::KeyUp(vk) => self.latch.key_up(vk),
            ScriptEvent::Tap(vk) => {
                self.latch.key_down(vk);
                self.latch.key_up(vk);
            }
            ScriptEvent::MouseMove(x, y) => self.latch.mouse_move(x, y),
            ScriptEvent::MouseDown(b) => self.latch.mouse_button(b),
            ScriptEvent::Quit => self.latch.request_quit(),
        }
    }
}

impl Host for HeadlessHost {
    fn present(&mut self, frame: Frame, pacing: Pacing) -> Result<InputState, Quit> {
        if self.quit {
            return Err(Quit);
        }
        self.frame_hashes.push(frame.stable_hash());
        self.pacings.push(pacing);
        if let Some(all) = &mut self.frames {
            all.push(frame.clone());
        }
        self.last_frame = Some(frame);

        let index = self.tick;
        while let Some(&(t, ev)) = self.script.get(self.cursor) {
            if t > index {
                break;
            }
            self.cursor += 1;
            self.apply(ev);
        }
        self.tick += 1;
        self.now_ms += u64::from(pacing.duration_ms());
        // The voice queue advances only when the engine asks (Host::pump_voice).
        self.mixer.advance_ms(pacing.duration_ms());

        if self.latch.quit_requested() {
            self.quit = true;
            return Err(Quit);
        }
        Ok(self.latch.take_tick())
    }

    /// Records the command and applies it to the mixer. The pause menu stops the sfx/voice
    /// channels and resumes them on P (F1.4), which arrives as `PauseChannel`.
    fn audio(&mut self, cmd: AudioCmd) {
        self.mixer.apply(cmd.clone(), &*self.source);
        self.audio_log.push((self.tick, cmd));
    }

    fn voice_busy(&self) -> bool {
        self.mixer.voice_busy()
    }

    fn voice_envelope(&self) -> u8 {
        self.mixer.voice_envelope()
    }

    fn sound_playing(&self, id: SoundId) -> bool {
        self.mixer.is_playing(id)
    }

    fn flush_input(&mut self) {
        self.latch.flush();
    }

    fn pump_voice(&mut self) {
        self.mixer.pump_voice();
    }

    fn now_ms(&self) -> u64 {
        self.now_ms
    }

    fn observe_debug(&mut self, state: &DebugState) {
        if let Some(mut r) = self.responder.take() {
            for ev in r(self.tick, state) {
                self.apply(ev);
            }
            self.responder = Some(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn headless_host_is_send() {
        assert_send::<HeadlessHost>();
    }
}
