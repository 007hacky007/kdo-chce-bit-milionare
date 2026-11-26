//! The context every screen function receives.

use std::fmt;

use render::{DrawOp, Frame};

use crate::audio_types::{AudioCmd, SoundId};
use crate::host::{Host, Pacing, Quit};
use crate::input::{InputState, MouseButton, Vk};
use crate::rng::Lcg;
use crate::sound_events::SoundPools;

/// Why every screen loop is unwinding (the original's `g_quitFlag`, F1.2 / F1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Abort {
    /// The window was closed. Sticky: every later present fails again.
    Quit,
    /// Pause menu N (0x322): unwind to `MainMenuLoop`, which catches it, calls
    /// [`Ctx::clear_new_game`] and starts over.
    NewGame,
}

impl From<Quit> for Abort {
    fn from(_: Quit) -> Self {
        Abort::Quit
    }
}

impl fmt::Display for Abort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Abort::Quit => f.write_str("quit requested"),
            Abort::NewGame => f.write_str("new game requested"),
        }
    }
}

impl std::error::Error for Abort {}

/// State exposed for tests and replays (e.g. a script answering with the correct slot).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugState {
    /// Slot (0..4) holding the correct answer of the current question.
    pub correct_slot: Option<usize>,
    /// Name of the screen loop currently running.
    pub screen: &'static str,
    pub money: [i32; 4],
    /// Millionaire (0..4, -1 = none) of every player, as of the last money sync.
    pub millionaire: [i32; 4],
    /// `game.round` answerer, beater and beaten as of the last money sync (the
    /// resolution and end of each question, the start and end of each beat-up round).
    pub answerer: Option<usize>,
    pub beater: Option<usize>,
    pub beaten: Option<usize>,
    /// `BeatUpLoop` state 0..5 (0x4244bc) while a beat-up round runs.
    pub beat_up_state: Option<u8>,
    /// The beater's fist is idle (anim 0) after this tick's update, so an arrow polled
    /// now starts a punch on the next tick.
    pub fist_idle: bool,
    /// The most recent screen entries as (presents so far, screen name), oldest first,
    /// capped at [`SCREEN_LOG_LEN`] entries.
    pub screen_log: Vec<(u64, &'static str)>,
}

/// Entries kept in [`DebugState::screen_log`].
pub const SCREEN_LOG_LEN: usize = 256;

/// What one `PumpMessages` (0x40fa10) call returns: the messages queued since the
/// previous pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pumped {
    /// Last key-down (`g_lastKey`).
    pub key: Option<Vk>,
    /// Mouse button message: left if any left-button press arrived, else the last one.
    pub mouse: Option<MouseButton>,
}

impl Pumped {
    fn merge(&mut self, input: &InputState) {
        if input.last_key.is_some() {
            self.key = input.last_key;
        }
        if self.mouse != Some(MouseButton::Left) && input.mouse_button.is_some() {
            self.mouse = input.mouse_button;
        }
    }
}

pub struct Ctx<'h> {
    host: &'h mut dyn Host,
    /// The single shared LCG (F2.4); every consumer draws from it in original order.
    pub rng: Lcg,
    /// Input of the tick that ended with the last present.
    pub input: InputState,
    /// Display list being composed for the next present.
    pub frame: Frame,
    pub debug: DebugState,
    /// `SoundEvent` pools and counters; live for the whole program run (F7.4).
    pub sound: SoundPools,
    pending: Option<Abort>,
    /// Messages delivered by presents since the last [`Ctx::pump`].
    queued: Pumped,
    /// State left by the last pump (`g_lastKey` 0x434304 and the mouse part of 0x43422c).
    pumped: Pumped,
    /// Copy of the last presented display list (the caller's canvas for pause/play again).
    last_presented: Frame,
    presents: u64,
}

impl<'h> Ctx<'h> {
    pub fn new(host: &'h mut dyn Host, seed: u32) -> Self {
        Self {
            host,
            rng: Lcg::new(seed),
            input: InputState::default(),
            frame: Frame::new(),
            debug: DebugState::default(),
            sound: SoundPools::new(),
            pending: None,
            queued: Pumped::default(),
            pumped: Pumped::default(),
            last_presented: Frame::new(),
            presents: 0,
        }
    }

    pub fn draw(&mut self, op: DrawOp) {
        self.frame.push(op);
    }

    /// Set the abort flag; the next present (in this loop and every enclosing one)
    /// returns it. `Quit` overrides a pending `NewGame` and can never be cleared.
    pub fn request_abort(&mut self, a: Abort) {
        if self.pending != Some(Abort::Quit) {
            self.pending = Some(a);
        }
    }

    pub fn pending_abort(&self) -> Option<Abort> {
        self.pending
    }

    /// `MainMenuLoop` clearing the quit flag after a pause-menu new game. A pending
    /// `Quit` is kept.
    pub fn clear_new_game(&mut self) {
        if self.pending == Some(Abort::NewGame) {
            self.pending = None;
        }
    }

    /// `PresentFrame66ms`: send the composed frame (leaving `frame` empty for the next
    /// tick), block until the tick ends, and store that tick's input. A pending abort is
    /// returned first, without presenting.
    pub fn present(&mut self) -> Result<(), Abort> {
        self.present_with(Pacing::Tick66)
    }

    /// Present a fullscreen cutscene frame ending `min_ms` (>= 66 ms) after the previous present's deadline.
    /// Compute `min_ms` from an absolute schedule, see [`Pacing::Cutscene`].
    pub fn present_cutscene(&mut self, min_ms: u32) -> Result<(), Abort> {
        self.present_with(Pacing::Cutscene { min_ms })
    }

    fn present_with(&mut self, pacing: Pacing) -> Result<(), Abort> {
        if let Some(a) = self.pending {
            return Err(a);
        }
        let frame = std::mem::take(&mut self.frame);
        self.last_presented = frame.clone();
        self.presents += 1;
        self.host.observe_debug(&self.debug);
        match self.host.present(frame, pacing) {
            Ok(input) => {
                self.queued.merge(&input);
                self.input = input;
                Ok(())
            }
            Err(Quit) => {
                self.pending = Some(Abort::Quit);
                Err(Abort::Quit)
            }
        }
    }

    /// `FlushInput` 0x40fb00 (used before cutscenes): drop `last_key`, the mouse button
    /// and latched taps, both in the current snapshot and in the host's pending events.
    pub fn flush_input(&mut self) {
        self.input.flush();
        self.host.flush_input();
        self.queued = Pumped::default();
        self.pumped = Pumped::default();
    }

    /// `PumpMessages` 0x40fa10: take the key and mouse messages delivered since the
    /// previous pump (across any number of presents, as Windows queues them) and make
    /// them the current [`last_key`](Self::last_key) / [`last_mouse`](Self::last_mouse).
    /// Screens call it exactly where the original pumps; [`Ctx::input`] stays the
    /// per-tick view for polled keys and the cursor.
    pub fn pump(&mut self) -> Pumped {
        self.pumped = std::mem::take(&mut self.queued);
        self.pumped
    }

    /// `g_lastKey` as left by the last pump (0 after a flush).
    pub fn last_key(&self) -> Option<Vk> {
        self.pumped.key
    }

    /// `g_lastKey = 0` (done by some screens after acting on a key).
    pub fn clear_last_key(&mut self) {
        self.pumped.key = None;
    }

    /// Mouse button message left by the last pump.
    pub fn last_mouse(&self) -> Option<MouseButton> {
        self.pumped.mouse
    }

    /// The display list most recently handed to the host.
    pub fn last_presented(&self) -> &Frame {
        &self.last_presented
    }

    /// Replace what [`Self::last_presented`] returns (the pause menu restores the
    /// caller's canvas after resuming).
    pub fn set_last_presented(&mut self, frame: Frame) {
        self.last_presented = frame;
    }

    /// Presents attempted so far.
    pub fn presents(&self) -> u64 {
        self.presents
    }

    /// Record entry into a screen loop in [`DebugState`]; returns the previous screen.
    pub fn enter_screen(&mut self, name: &'static str) -> &'static str {
        let log = &mut self.debug.screen_log;
        if log.len() >= SCREEN_LOG_LEN {
            log.remove(0);
        }
        log.push((self.presents, name));
        std::mem::replace(&mut self.debug.screen, name)
    }

    pub fn audio(&mut self, cmd: AudioCmd) {
        self.host.audio(cmd);
    }

    /// `VoiceQueuePump` 0x40b4a0: start the next queued voice clip if the voice slot is
    /// free. Nothing pumps the queue implicitly: screens call this exactly where the
    /// original calls 0x40b4a0 (in most loops once per tick right after the present; not
    /// in cutscenes, the pause slide-in or the pause menu).
    pub fn pump_voice(&mut self) {
        self.host.pump_voice();
    }

    pub fn voice_busy(&self) -> bool {
        self.host.voice_busy()
    }

    pub fn voice_envelope(&self) -> u8 {
        self.host.voice_envelope()
    }

    pub fn sound_playing(&self, id: SoundId) -> bool {
        self.host.sound_playing(id)
    }

    pub fn now_ms(&self) -> u64 {
        self.host.now_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{HeadlessHost, ScriptEvent};

    #[test]
    fn pump_takes_keys_queued_since_previous_pump() {
        let script = vec![
            (0, ScriptEvent::Tap(Vk::A)),
            (0, ScriptEvent::MouseDown(MouseButton::Left)),
            (0, ScriptEvent::MouseDown(MouseButton::Right)),
            (1, ScriptEvent::MouseDown(MouseButton::Right)),
            (3, ScriptEvent::Tap(Vk::B)),
        ];
        let mut host = HeadlessHost::with_script(script);
        let mut ctx = Ctx::new(&mut host, 1);
        ctx.present().unwrap();
        ctx.present().unwrap();
        assert_eq!(ctx.last_key(), None);
        let p = ctx.pump();
        assert_eq!(p.key, Some(Vk::A));
        // A left press wins over later right presses, in its own tick and across ticks
        // (0x40fa10).
        assert_eq!(p.mouse, Some(MouseButton::Left));
        assert_eq!(ctx.pump(), Pumped::default());
        ctx.present().unwrap();
        ctx.present().unwrap();
        ctx.flush_input();
        assert_eq!(ctx.pump(), Pumped::default());
        assert_eq!(ctx.presents(), 4);
        assert_eq!(ctx.last_presented(), &Frame::new());
    }

    #[test]
    fn screen_log_is_capped() {
        let mut host = HeadlessHost::with_script(vec![]);
        let mut ctx = Ctx::new(&mut host, 1);
        for _ in 0..SCREEN_LOG_LEN + 5 {
            ctx.enter_screen("A");
        }
        assert_eq!(ctx.enter_screen("B"), "A");
        assert_eq!(ctx.debug.screen_log.len(), SCREEN_LOG_LEN);
        assert_eq!(ctx.debug.screen_log.last(), Some(&(0, "B")));
    }
}
