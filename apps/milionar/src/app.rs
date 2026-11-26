//! The app's two modes: setup (no usable import yet) and the game.
//!
//! The window and event loop exist before any game data is opened. [`startup`] restores
//! an interrupted import and decides the first mode; [`GameRunner`] owns everything that
//! only exists while a game runs (the game thread, its host state, the mixer and audio
//! device, the shared playing lock), so it can be torn down and started again, for
//! example around a re-import.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use engine::{Abort, Assets, ClipCache, Ctx, FrameSnapshot, Game, Mixer, SharedFrames};
use gamedata::GameData;
use render::Frame;
use setup::{ImportError, PlayingLock};

use crate::audio_out::{self, AudioOut, AudioStats};
use crate::host::{GameHost, Shared, Wake, lock};

/// How long the main thread waits for the engine to unwind after a quit.
const QUIT_GRACE: Duration = Duration::from_secs(3);
/// How long [`GameRunner::shutdown`] waits for the game thread to finish.
const JOIN_WAIT: Duration = Duration::from_millis(500);

/// Why the app starts (or stays) in setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupReason {
    /// No completed import (`game/import.json` is missing).
    NoImport,
    /// An import holds the data dir (another instance is importing).
    ImportRunning,
    /// The data dir or the import cannot be read; the text says why.
    Unreadable(String),
    /// The options asked to import again (the game was shut down for it).
    Reimport,
    /// The options asked to make the enhanced graphics again (the game was shut down).
    Rebuild,
}

impl SetupReason {
    /// A line for the log.
    pub fn describe(&self, data_dir: &Path) -> String {
        let dir = data_dir.display();
        match self {
            SetupReason::NoImport => format!("setup: no imported game data in {dir}"),
            SetupReason::ImportRunning => format!("setup: an import is running for {dir}"),
            SetupReason::Unreadable(why) => format!("setup: cannot use the data in {dir}: {why}"),
            SetupReason::Reimport => format!("setup: importing the game data into {dir} again"),
            SetupReason::Rebuild => {
                format!("setup: making the enhanced graphics for {dir} again")
            }
        }
    }
}

/// The two stores a game needs: frames (in the game) and sounds (in the host), and the
/// playing lock taken before they were opened.
#[derive(Debug)]
pub struct GameFiles {
    pub data: GameData,
    pub sound: GameData,
    /// Keeps an import from replacing `game/` from the moment it is opened (`None`
    /// when no lock file can be made there, in which case no import can run either).
    pub playing: Option<PlayingLock>,
}

/// The first mode, from what is in the data dir.
#[derive(Debug)]
pub enum Startup {
    Setup(SetupReason),
    Game(Box<GameFiles>),
}

/// Restores an interrupted import, takes the playing lock, then checks for a completed
/// import. The cheap "imported" check is `game/import.json` (written last by the
/// importer); no re-hash.
pub fn startup(data_dir: &Path) -> Startup {
    match setup::recover(data_dir) {
        Ok(()) => {}
        Err(ImportError::Busy(_)) => return Startup::Setup(SetupReason::ImportRunning),
        Err(e) => {
            return Startup::Setup(SetupReason::Unreadable(format!(
                "cannot restore the data directory: {e}"
            )));
        }
    }
    // The playing lock comes before anything is read, so an import cannot swap `game/`
    // between the checks below and the game; setup drops it again (its own import
    // takes it exclusively).
    let playing = match PlayingLock::acquire(data_dir) {
        Ok(l) => Some(l),
        Err(ImportError::Busy(_)) => return Startup::Setup(SetupReason::ImportRunning),
        Err(e) => {
            crate::log::log(&format!("warning: no playing lock ({e})"));
            None
        }
    };
    if !data_dir.join("game").join("import.json").is_file() {
        return Startup::Setup(SetupReason::NoImport);
    }
    match (GameData::open(data_dir), GameData::open(data_dir)) {
        (Ok(data), Ok(sound)) => Startup::Game(Box::new(GameFiles {
            data,
            sound,
            playing,
        })),
        (Err(e), _) | (_, Err(e)) => Startup::Setup(SetupReason::Unreadable(e.to_string())),
    }
}

/// What a game needs to start.
pub struct GameStart<'a> {
    pub data_dir: &'a Path,
    pub files: GameFiles,
    pub debug_cheats: bool,
    pub seed: u32,
    /// Master volume 0..=100.
    pub volume: u32,
    /// Hash frames for the upscaled pack (only worth it when a pack is open).
    pub hashing: bool,
}

/// How a running game ended.
#[derive(Debug, PartialEq, Eq)]
pub enum GameEnd {
    /// The engine returned; the text is a reason worth logging.
    Finished(Option<String>),
    /// The game thread is gone without reporting.
    Crashed,
    /// A quit was requested and the engine did not unwind within [`QUIT_GRACE`].
    QuitTimeout,
}

/// A running game: the game thread and the main-thread state that lives with it.
pub struct GameRunner {
    shared: Arc<Shared>,
    /// The frame mirror's handle (a clone of the game thread's): content hashing for
    /// the upscaled pack is switched here.
    frames: SharedFrames,
    #[cfg(feature = "test-hooks")]
    mixer: Arc<Mutex<Mixer>>,
    #[cfg(feature = "test-hooks")]
    audio_stats: Arc<AudioStats>,
    /// Dropped before the mixer goes (see `audio_out::AudioOut`).
    audio: Option<AudioOut>,
    thread: JoinHandle<()>,
    asset_errors: Arc<Mutex<Vec<String>>>,
    quit_at: Option<Instant>,
    /// Keeps an import from replacing `game/` while this game reads it.
    _playing: Option<PlayingLock>,
}

impl GameRunner {
    /// Opens the audio device and starts the engine on a game thread; keeps the files'
    /// playing lock. `wake` is called for every new frame.
    pub fn start(start: GameStart<'_>, sdl: &sdl3::Sdl, wake: Wake) -> Result<GameRunner, String> {
        let playing = start.files.playing;
        let mixer = Arc::new(Mutex::new(Mixer::new()));
        let audio_stats = Arc::new(AudioStats::default());
        let audio = match sdl.audio() {
            Ok(a) => audio_out::open(&a, mixer.clone(), start.volume, audio_stats.clone()),
            Err(e) => {
                eprintln!("warning: no audio ({e}); playing silently");
                None
            }
        };

        // Assets live in Game; the host reads frames through the shared mirror and
        // sounds through a second store on the same data (sound lookup needs no frames).
        let mut assets = Assets::new(start.files.data);
        let frames = assets.share_frames();
        frames.set_hashing(start.hashing);
        let mut game = Game::new(assets).with_saves_dir(start.data_dir.join("saves"));
        game.debug_cheats = start.debug_cheats;
        let clips = ClipCache::new(Box::new(Assets::new(start.files.sound)));
        let shared = Arc::new(Shared::default());
        let host = GameHost::new(
            wake,
            shared.clone(),
            frames.clone(),
            mixer.clone(),
            audio.is_some(),
            clips,
        );
        let seed = start.seed;
        let asset_errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let errors_out = asset_errors.clone();
        let thread = std::thread::Builder::new()
            .name("game".into())
            .stack_size(64 << 20)
            .spawn(move || {
                let mut host = host;
                let result = {
                    let mut ctx = Ctx::new(&mut host, seed);
                    engine::run(&mut ctx, &mut game)
                };
                {
                    let mut errs = lock(&errors_out);
                    for e in &game.asset_errors {
                        let line = format!("asset error: {e}");
                        crate::log::log(&line);
                        errs.push(line);
                    }
                }
                host.done(match result {
                    Ok(()) | Err(Abort::Quit) => None,
                    Err(e) => Some(e.to_string()),
                });
            })
            .map_err(|e| format!("cannot start the game thread: {e}"))?;
        Ok(GameRunner {
            shared,
            frames,
            #[cfg(feature = "test-hooks")]
            mixer,
            #[cfg(feature = "test-hooks")]
            audio_stats,
            audio,
            thread,
            asset_errors,
            quit_at: None,
            _playing: playing,
        })
    }

    /// Input and quit state shared with the game thread.
    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    /// Content hashing for the upscaled pack, from the next snapshot on. Turn it on
    /// before a pack is installed and the art switched to it, so the first upscaled
    /// frame already has its identity.
    pub fn set_hashing(&self, on: bool) {
        self.frames.set_hashing(on);
    }

    /// Pauses (the options overlay is open) or resumes the game and its sound. The game
    /// thread blocks in its next present; see `host.rs` for how timing is kept.
    pub fn set_paused(&self, on: bool) {
        self.shared.set_paused(on);
        if let Some(a) = &self.audio {
            a.set_paused(on);
        }
    }

    /// Master volume 0..=100.
    pub fn set_volume(&self, volume: u32) {
        if let Some(a) = &self.audio {
            a.set_volume(volume);
        }
    }

    #[cfg(feature = "test-hooks")]
    pub fn mixer(&self) -> &Arc<Mutex<Mixer>> {
        &self.mixer
    }

    #[cfg(feature = "test-hooks")]
    pub fn audio_stats(&self) -> &AudioStats {
        &self.audio_stats
    }

    /// Asks the engine to unwind (window close, Cmd+Q); [`poll`](Self::poll) reports
    /// the end.
    pub fn request_quit(&mut self) {
        self.shared.request_quit();
        self.quit_at.get_or_insert_with(Instant::now);
    }

    /// The newest frame, if one arrived, and whether the game has ended.
    pub fn poll(&mut self) -> (Option<(Frame, FrameSnapshot)>, Option<GameEnd>) {
        let (frame, done) = {
            let mut mb = lock(&self.shared.mailbox);
            (mb.frame.take(), mb.done.take())
        };
        let end = if let Some(reason) = done {
            Some(GameEnd::Finished(reason))
        } else if self.thread.is_finished() {
            Some(GameEnd::Crashed)
        } else if self.quit_at.is_some_and(|t| t.elapsed() > QUIT_GRACE) {
            Some(GameEnd::QuitTimeout)
        } else {
            None
        };
        (frame, end)
    }

    /// Stops the game (asking it to quit if it still runs), waits briefly for the game
    /// thread, closes the audio device and releases the playing lock. Returns the
    /// asset errors the game reported.
    ///
    /// A game thread that does not finish in time is left running (the process is
    /// normally exiting) and keeps the playing lock until the process ends, so no
    /// import can replace `game/` under it.
    pub fn shutdown(self) -> Vec<String> {
        let GameRunner {
            shared,
            audio,
            thread,
            asset_errors,
            _playing: playing,
            ..
        } = self;
        shared.request_quit();
        let deadline = Instant::now() + JOIN_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
            drop(playing);
        } else {
            crate::log::log("warning: the game thread did not stop; keeping the playing lock");
            std::mem::forget(playing);
        }
        drop(audio);
        std::mem::take(&mut *lock(&asset_errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_mode_selection() {
        let tmp = tempfile::tempdir().unwrap();
        // First run: the data dir does not exist yet.
        let fresh = tmp.path().join("nová složka");
        assert!(matches!(
            startup(&fresh),
            Startup::Setup(SetupReason::NoImport)
        ));
        // A game dir without import.json is an interrupted or foreign copy.
        let d = tmp.path().join("data");
        std::fs::create_dir_all(d.join("game")).unwrap();
        std::fs::write(d.join("game/Milionar.exe"), b"x").unwrap();
        assert!(matches!(startup(&d), Startup::Setup(SetupReason::NoImport)));
        std::fs::write(d.join("game/import.json"), b"{}").unwrap();
        assert!(matches!(startup(&d), Startup::Game(_)));
        // An import in progress elsewhere: setup (it waits), not a crash.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(d.join(".import.lock"))
            .unwrap();
        fs4::FileExt::try_lock(&lock).unwrap();
        assert!(matches!(
            startup(&d),
            Startup::Setup(SetupReason::ImportRunning)
        ));
        drop(lock);
        // Recovery runs first: a crash between the promote renames is undone.
        std::fs::create_dir_all(d.join(".old-7")).unwrap();
        std::fs::rename(d.join("game"), d.join(".old-7/game")).unwrap();
        assert!(matches!(startup(&d), Startup::Game(_)));
        assert!(d.join("game/import.json").is_file());
        // An import whose files cannot be indexed is set up again.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let sub = d.join("game/Menus");
            std::fs::create_dir(&sub).unwrap();
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
            let st = startup(&d);
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                matches!(&st, Startup::Setup(SetupReason::Unreadable(m)) if m.contains("cannot index")),
                "{st:?}"
            );
        }
        std::fs::remove_dir_all(d.join("game")).unwrap();
        std::fs::write(d.join("game"), b"not a dir").unwrap();
        assert!(matches!(startup(&d), Startup::Setup(SetupReason::NoImport)));
    }

    fn try_exclusive(d: &Path) -> bool {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(d.join(".playing.lock"))
            .unwrap();
        fs4::FileExt::try_lock(&f).is_ok()
    }

    /// The playing lock is taken before the game data is opened and travels with the
    /// files into the game, so an import cannot swap `game/` between the two.
    #[test]
    fn startup_takes_the_playing_lock_first() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data s mezerou");
        std::fs::create_dir_all(d.join("game")).unwrap();
        std::fs::write(d.join("game/import.json"), b"{}").unwrap();
        let Startup::Game(files) = startup(&d) else {
            panic!("expected game mode");
        };
        assert!(files.playing.is_some());
        assert!(!try_exclusive(&d), "an import could take the lock");
        drop(files);
        assert!(try_exclusive(&d));
        // Setup mode holds no lock (its own import needs it exclusively).
        std::fs::remove_file(d.join("game/import.json")).unwrap();
        assert!(matches!(startup(&d), Startup::Setup(SetupReason::NoImport)));
        assert!(try_exclusive(&d));
        // An import holding the playing lock (after the import lock): setup, waiting.
        std::fs::write(d.join("game/import.json"), b"{}").unwrap();
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(d.join(".playing.lock"))
            .unwrap();
        fs4::FileExt::try_lock(&held).unwrap();
        assert!(matches!(
            startup(&d),
            Startup::Setup(SetupReason::ImportRunning)
        ));
    }
}
