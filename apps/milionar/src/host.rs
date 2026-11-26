//! The game-thread side of the SDL host.
//!
//! Threads: the main thread owns the window, renderer, audio device and event loop; the
//! engine runs on a game thread and talks to it through [`GameHost`]:
//!
//! - `present` resolves every `DrawOp::Smk` of the frame through [`SharedFrames`] into an
//!   owned [`FrameSnapshot`] (still on the game thread, so the references are valid),
//!   puts frame plus snapshot into the [`Mailbox`] (replacing one the main thread has not
//!   picked up yet) and wakes the main thread, then sleeps out the pacing and takes
//!   the tick's input from the shared [`InputAccumulator`] that the main thread feeds.
//! - `audio` resolves the clip with a [`ClipCache`] outside the mixer lock, then locks the
//!   shared [`Mixer`] briefly and applies the command, so `voice_busy`, `voice_envelope`
//!   and `sound_playing` reflect it at once. The device callback only locks to render.
//!   Without a device the mixer is advanced by wall-clock time in `present`.
//! - Quit: the main thread sets [`Shared::quit`] (window close, Cmd+Q, Alt+F4); a present
//!   that is waiting returns `Err(Quit)` within a few milliseconds and the engine unwinds.
//! - Pause (the options overlay): the main thread calls [`Shared::set_paused`]. A present
//!   that is waiting (or the next one, after delivering its frame) blocks until the pause
//!   ends or a quit. Its deadline moves by the paused time, so after the resume the tick
//!   or cutscene hold in progress gets only its remaining time and the 66 ms schedule
//!   continues from there (no catch-up burst, no cut-short hold). `now_ms` leaves the
//!   paused time out (cutscene deadlines do not expire during the pause), and without an
//!   audio device the mixer is not advanced for it. With a device the app pauses the
//!   device itself, so voices resume where they stopped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use engine::{
    AudioCmd, ClipCache, FrameSnapshot, Host, InputAccumulator, InputState, Mixer, Pacing, Quit,
    SharedFrames, SoundId, command_sound,
};
use render::Frame;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the game thread leaves for the main thread. Only the newest frame is kept: a
/// frame the main thread has not picked up yet is simply replaced.
#[derive(Default)]
pub struct Mailbox {
    pub frame: Option<(Frame, FrameSnapshot)>,
    /// Set once the engine has returned (normally after a quit); the inner text is a
    /// reason worth logging.
    pub done: Option<Option<String>>,
}

/// State both threads touch.
#[derive(Default)]
pub struct Shared {
    pub input: Mutex<InputAccumulator>,
    /// Set by the main thread when the user closes the window.
    pub quit: AtomicBool,
    pub mailbox: Mutex<Mailbox>,
    /// Set while the options overlay is open; see the module docs.
    paused: Mutex<bool>,
    resumed: Condvar,
    /// Mirrors `paused` for the waiting loop's cheap check.
    pause_flag: AtomicBool,
}

impl Shared {
    pub fn request_quit(&self) {
        lock(&self.input).request_quit();
        self.quit.store(true, Ordering::SeqCst);
        self.resumed.notify_all();
    }

    /// Pauses or resumes the game thread (at its next present).
    pub fn set_paused(&self, on: bool) {
        *lock(&self.paused) = on;
        self.pause_flag.store(on, Ordering::SeqCst);
        self.resumed.notify_all();
    }

    /// Blocks while paused (until resumed or a quit); how long, if it blocked.
    fn wait_while_paused(&self) -> Option<Duration> {
        let mut paused = lock(&self.paused);
        if !*paused {
            return None;
        }
        let t0 = Instant::now();
        while *paused && !self.quit.load(Ordering::SeqCst) {
            paused = self
                .resumed
                .wait_timeout(paused, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Some(t0.elapsed())
    }
}

/// Wakes the main thread's event wait (an SDL user event in the app).
pub type Wake = Box<dyn Fn() + Send>;

pub struct GameHost {
    wake: Wake,
    shared: Arc<Shared>,
    frames: SharedFrames,
    mixer: Arc<Mutex<Mixer>>,
    /// An audio device renders the mixer; without one, `present` advances it.
    has_device: bool,
    clips: ClipCache,
    start: Instant,
    last_present: Instant,
    last_advance: Instant,
    /// Time spent paused, left out of `now_ms`.
    paused_total: Duration,
    quit: bool,
    #[cfg(feature = "test-hooks")]
    ticks: TickLog,
}

/// Pacing check for scripted runs: how late each present's wait ended, logged to stderr
/// every 100 presents (`ticks: ...`).
#[cfg(feature = "test-hooks")]
#[derive(Default)]
struct TickLog {
    n: u32,
    late: u32,
    max_late: Duration,
}

#[cfg(feature = "test-hooks")]
impl TickLog {
    fn add(&mut self, late: Duration) {
        self.n += 1;
        self.max_late = self.max_late.max(late);
        if late > Duration::from_millis(5) {
            self.late += 1;
        }
        if self.n == 100 {
            eprintln!(
                "ticks: 100 presents, {} late by > 5 ms, max late {:.2} ms",
                self.late,
                self.max_late.as_secs_f64() * 1000.0
            );
            *self = TickLog::default();
        }
    }
}

impl GameHost {
    pub fn new(
        wake: Wake,
        shared: Arc<Shared>,
        frames: SharedFrames,
        mixer: Arc<Mutex<Mixer>>,
        has_device: bool,
        clips: ClipCache,
    ) -> Self {
        let now = Instant::now();
        Self {
            wake,
            shared,
            frames,
            mixer,
            has_device,
            clips,
            start: now,
            last_present: now,
            last_advance: now,
            paused_total: Duration::ZERO,
            quit: false,
            #[cfg(feature = "test-hooks")]
            ticks: TickLog::default(),
        }
    }

    /// Tell the main thread the engine has finished.
    pub fn done(&self, reason: Option<String>) {
        lock(&self.shared.mailbox).done = Some(reason);
        (self.wake)();
    }

    fn quit_now(&mut self) -> Result<InputState, Quit> {
        self.quit = true;
        Err(Quit)
    }
}

/// Sleep until `deadline`, waking early if `quit` (or `pause`) is set. Sleeps in slices
/// of at most 5 ms and spins through the last millisecond for precise ticks.
pub fn wait_until(deadline: Instant, quit: &AtomicBool, pause: &AtomicBool) {
    loop {
        if quit.load(Ordering::Relaxed) || pause.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > Duration::from_micros(1500) {
            std::thread::sleep((left - Duration::from_millis(1)).min(Duration::from_millis(5)));
        } else {
            std::hint::spin_loop();
        }
    }
}

impl Host for GameHost {
    fn present(&mut self, frame: Frame, pacing: Pacing) -> Result<InputState, Quit> {
        if self.quit || self.shared.quit.load(Ordering::SeqCst) {
            return self.quit_now();
        }
        let snapshot = self.frames.snapshot(&frame);
        lock(&self.shared.mailbox).frame = Some((frame, snapshot));
        (self.wake)();
        let mut deadline =
            self.last_present + Duration::from_millis(u64::from(pacing.duration_ms()));
        loop {
            wait_until(deadline, &self.shared.quit, &self.shared.pause_flag);
            if self.shared.quit.load(Ordering::SeqCst) {
                break;
            }
            match self.shared.wait_while_paused() {
                // The pause is not game or mixer time: the tick in progress keeps the
                // rest of its time for after the resume.
                Some(d) => {
                    self.paused_total += d;
                    self.last_advance += d;
                    deadline += d;
                }
                None => break,
            }
        }
        let now = Instant::now();
        #[cfg(feature = "test-hooks")]
        self.ticks.add(now.saturating_duration_since(deadline));
        // On time: keep the exact schedule. Late (a slow load): measure from now.
        self.last_present = if now > deadline + Duration::from_millis(5) {
            now
        } else {
            deadline
        };
        if !self.has_device {
            let ms = (now - self.last_advance).as_millis() as u32;
            self.last_advance += Duration::from_millis(u64::from(ms));
            lock(&self.mixer).advance_ms(ms);
        }
        let mut input = lock(&self.shared.input);
        if input.quit_requested() || self.shared.quit.load(Ordering::SeqCst) {
            drop(input);
            return self.quit_now();
        }
        Ok(input.take_tick())
    }

    fn audio(&mut self, cmd: AudioCmd) {
        // Decode (first use) outside the mixer lock.
        let clip = command_sound(&cmd).and_then(|id| self.clips.get(id));
        lock(&self.mixer).apply_clip(cmd, clip);
    }

    fn voice_busy(&self) -> bool {
        lock(&self.mixer).voice_busy()
    }

    fn voice_envelope(&self) -> u8 {
        lock(&self.mixer).voice_envelope()
    }

    fn sound_playing(&self, id: SoundId) -> bool {
        lock(&self.mixer).is_playing(id)
    }

    fn flush_input(&mut self) {
        lock(&self.shared.input).flush();
    }

    fn pump_voice(&mut self) {
        lock(&self.mixer).pump_voice();
    }

    fn now_ms(&self) -> u64 {
        self.start
            .elapsed()
            .saturating_sub(self.paused_total)
            .as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{SilentSource, Vk};
    use std::sync::atomic::AtomicUsize;

    fn host(has_device: bool) -> (GameHost, Arc<AtomicUsize>, Arc<Shared>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let shared = Arc::new(Shared::default());
        let h = GameHost::new(
            Box::new(move || {
                w.fetch_add(1, Ordering::SeqCst);
            }),
            shared.clone(),
            SharedFrames::default(),
            Arc::new(Mutex::new(Mixer::new())),
            has_device,
            ClipCache::new(Box::new(SilentSource)),
        );
        (h, wakes, shared)
    }

    #[test]
    fn present_paces_ticks_and_delivers_input() {
        let (mut h, wakes, shared) = host(false);
        h.present(Frame::new(), Pacing::Tick66).unwrap();
        let t0 = Instant::now();
        lock(&shared.input).key_down(Vk::A);
        lock(&shared.input).key_up(Vk::A);
        let input = h.present(Frame::new(), Pacing::Tick66).unwrap();
        let dt = t0.elapsed();
        assert!(
            // Generous upper bound: loaded CI machines oversleep.
            dt >= Duration::from_millis(60) && dt < Duration::from_millis(150),
            "{dt:?}"
        );
        assert_eq!(input.last_key, Some(Vk::A));
        assert!(input.held.contains(&Vk::A));
        // Each present woke the main thread; only the newest frame waits in the mailbox.
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        let mut mb = lock(&shared.mailbox);
        assert!(mb.frame.take().is_some());
        assert!(mb.done.is_none());
        drop(mb);
        h.done(None);
        assert_eq!(lock(&shared.mailbox).done, Some(None));
        assert_eq!(wakes.load(Ordering::SeqCst), 3);
    }

    /// Pausing blocks the game thread in `present` until resumed; the pacing restarts
    /// from the resume (no burst of catch-up ticks) and the paused time is not game time.
    #[test]
    fn pause_blocks_present_and_restarts_pacing() {
        let (mut h, _w, shared) = host(false);
        h.present(Frame::new(), Pacing::Tick66).unwrap();
        let t0 = h.now_ms();
        shared.set_paused(true);
        let s = shared.clone();
        let resumer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            assert!(
                lock(&s.mailbox).frame.is_some(),
                "the frame before the pause arrived"
            );
            s.set_paused(false);
        });
        let started = Instant::now();
        h.present(Frame::new(), Pacing::Tick66).unwrap();
        let blocked = started.elapsed();
        assert!(blocked >= Duration::from_millis(290), "{blocked:?}");
        resumer.join().unwrap();
        // The next tick is a full 66 ms after the resume, not due at once.
        let t1 = Instant::now();
        h.present(Frame::new(), Pacing::Tick66).unwrap();
        let dt = t1.elapsed();
        assert!(dt >= Duration::from_millis(60), "{dt:?}");
        // Game time left out the pause.
        let game = h.now_ms() - t0;
        assert!(game < 250, "{game} ms of game time");
        // A pause in the middle of a long hold (a cutscene still) keeps the rest of the
        // hold for after the resume.
        let s = shared.clone();
        let resumer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            s.set_paused(true);
            std::thread::sleep(Duration::from_millis(300));
            s.set_paused(false);
        });
        let t3 = Instant::now();
        h.present(Frame::new(), Pacing::Cutscene { min_ms: 400 })
            .unwrap();
        let held = t3.elapsed();
        resumer.join().unwrap();
        assert!(held >= Duration::from_millis(680), "{held:?}");
        // Quit wakes a paused present.
        shared.set_paused(true);
        let s = shared.clone();
        let q = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            s.request_quit();
        });
        let t2 = Instant::now();
        assert_eq!(h.present(Frame::new(), Pacing::Tick66), Err(Quit));
        assert!(t2.elapsed() < Duration::from_millis(500));
        q.join().unwrap();
    }

    #[test]
    fn quit_interrupts_a_waiting_present() {
        let (mut h, _rx, shared) = host(false);
        let s = shared.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            s.request_quit();
        });
        let t0 = Instant::now();
        assert_eq!(
            h.present(Frame::new(), Pacing::Cutscene { min_ms: 5000 }),
            Err(Quit)
        );
        assert!(t0.elapsed() < Duration::from_millis(500));
        assert_eq!(h.present(Frame::new(), Pacing::Tick66), Err(Quit));
        t.join().unwrap();
    }

    #[test]
    fn audio_commands_take_effect_synchronously() {
        let pcm = Arc::new(formats::wav::Pcm {
            sample_rate: 22050,
            channels: 1,
            bits_per_sample: 16,
            samples: vec![100; 22050],
        });
        let source = move |_id: SoundId| Some(pcm.clone());
        let mut h = GameHost::new(
            Box::new(|| {}),
            Arc::new(Shared::default()),
            SharedFrames::default(),
            Arc::new(Mutex::new(Mixer::new())),
            true,
            ClipCache::new(Box::new(source)),
        );
        let id = SoundId::new(engine::Prefix::Char, 1);
        h.audio(AudioCmd::QueueVoice(id));
        assert!(h.voice_busy(), "queued voice counts as busy at once");
        h.pump_voice();
        assert!(h.sound_playing(id));
        h.audio(AudioCmd::StopVoice);
        assert!(!h.voice_busy());
    }
}
