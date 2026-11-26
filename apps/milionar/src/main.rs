//! Desktop app: SDL3 window, audio and input around the engine.
//!
//! The main thread runs SDL (window, renderer, event loop, audio device). The window and
//! event loop come first; then the app runs in one of two modes ([`app::Mode`]): setup
//! (no usable import yet) or the game, whose engine runs on a game thread behind
//! [`host::GameHost`] (see `host.rs` for the threading contract).

// Release builds on Windows are GUI programs (no console window).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod art;
mod audio_out;
mod host;
mod keys;
mod layout;
mod log;
mod options;
mod settings;
mod setup_ui;
mod strings;
#[cfg(feature = "test-hooks")]
mod testhook;
mod video;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use engine::{FrameSnapshot, MouseButton};
use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::{Keycode, Mod, Scancode};
use sdl3::messagebox::{MessageBoxFlag, show_simple_message_box};
use sdl3::video::FullscreenType;

use crate::app::{GameEnd, GameFiles, GameRunner, GameStart, SetupReason};
use crate::art::PackKind;
use crate::host::{Wake, lock};
use crate::options::{Overlay, Place};
use crate::settings::{Settings, settings_path};
use crate::setup_ui::{Ctx, Nav, Outcome, SetupMode};
use crate::strings::Lang;

const TITLE: &str = "Kdo chce bít milionáře";
/// Longest idle wait for events; the game thread wakes the loop for every new frame.
const IDLE_WAIT: Duration = Duration::from_millis(50);
/// Settings changed by resizing or fullscreen are saved this long after the last change.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// SDL user event the game thread pushes when a new frame is in the mailbox.
struct FrameReady;

/// What the app is doing.
enum Mode {
    Setup(Box<SetupMode>),
    Game(GameRunner),
}

#[derive(Parser, Debug)]
#[command(name = "milionar", about = "Kdo chce bít milionáře (native port)")]
struct Args {
    /// Data directory holding the imported game (`game/`), saves and settings.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Start in a window (overrides the saved setting).
    #[arg(long, conflicts_with = "fullscreen")]
    windowed: bool,
    /// Start fullscreen (overrides the saved setting).
    #[arg(long)]
    fullscreen: bool,
    /// Enable the original's beat-up cheats (S+W special round, K+O knockout).
    #[arg(long)]
    debug_cheats: bool,
    /// RNG seed (testing only; default: Unix time in seconds).
    #[arg(long, value_name = "N")]
    seed: Option<u32>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(gamedata::default_data_dir);
    log::init(&data_dir);
    match run(&args, &data_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            show_error(&msg);
            ExitCode::FAILURE
        }
    }
}

fn headless() -> bool {
    std::env::var("SDL_VIDEODRIVER")
        .map(|v| matches!(v.as_str(), "dummy" | "offscreen"))
        .unwrap_or(false)
}

/// Message box plus stderr and log file. Headless video drivers (tests, CI) get no message
/// box, so it can never block an unattended run.
fn show_error(msg: &str) {
    log::log(&format!("{TITLE}: {msg}"));
    if !headless()
        && let Err(e) = show_simple_message_box(MessageBoxFlag::ERROR, TITLE, msg, None)
    {
        eprintln!("(cannot show a message box: {e})");
    }
}

fn seed(args: &Args) -> u32 {
    args.seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0)
    })
}

/// Shows the setup wizard.
fn enter_setup(
    reason: SetupReason,
    data_dir: &Path,
    lang: Lang,
    screen: &mut video::Screen,
) -> Mode {
    log::log(&reason.describe(data_dir));
    screen.set_pack(None);
    Mode::Setup(Box::new(SetupMode::new(lang, reason, data_dir)))
}

/// Opens the upscaled pack and starts the game (the files hold the playing lock). Also
/// returns what was found in `upscaled/` (the options explain it).
fn start_game(
    args: &Args,
    data_dir: &Path,
    files: GameFiles,
    sdl: &sdl3::Sdl,
    events: &sdl3::EventSubsystem,
    screen: &mut video::Screen,
    settings: &Settings,
) -> Result<(GameRunner, PackKind), String> {
    // Upscaled art: validated once here; any problem is logged and the original art used.
    let t0 = Instant::now();
    let status = art::open_pack(data_dir);
    let line = status.describe(data_dir);
    let kind = status.kind();
    let pack = status.into_pack();
    if pack.is_some() {
        log::log(&format!(
            "{line} (opened in {} ms)",
            t0.elapsed().as_millis()
        ));
    } else {
        log::log(&line);
    }
    let hashing = pack.is_some();
    let sender = events.event_sender();
    let start = GameStart {
        data_dir,
        files,
        debug_cheats: args.debug_cheats,
        seed: seed(args),
        volume: settings.volume,
        hashing,
    };
    let wake = Box::new(move || {
        // A full queue only delays the frame to the next idle wakeup.
        let _ = sender.push_custom_event(FrameReady);
    });
    let runner = GameRunner::start(start, sdl, wake)?;
    install_pack(&runner, screen, pack, settings.art_mode);
    Ok((runner, kind))
}

/// Gives the screen the pack and the art setting. Content hashing is switched first, so
/// the first upscaled frame already carries its identity.
fn install_pack(
    runner: &GameRunner,
    screen: &mut video::Screen,
    pack: Option<std::sync::Arc<render::upscaled::UpscaledPack>>,
    art: settings::ArtMode,
) {
    runner.set_hashing(pack.is_some());
    screen.set_pack(pack);
    screen.set_art(art);
    log::log(&format!(
        "art mode: {} (setting {})",
        screen.art().name(),
        art.name()
    ));
}

fn run(args: &Args, data_dir: &Path) -> Result<(), String> {
    let settings_file = settings_path(data_dir);
    let mut settings = Settings::load(&settings_file);
    if args.fullscreen {
        settings.fullscreen = true;
    }
    if args.windowed {
        settings.fullscreen = false;
    }

    // Fullscreen without a macOS Space: instant toggle (no animation during which the
    // window state lags), the same borderless-desktop behaviour as on Windows. An
    // environment variable of the same name still overrides it.
    sdl3::hint::set("SDL_VIDEO_MAC_FULLSCREEN_SPACES", "0");
    let sdl = sdl3::init().map_err(|e| format!("SDL init failed: {e}"))?;
    let video = sdl
        .video()
        .map_err(|e| format!("SDL video init failed: {e}"))?;
    let (w, h) = fit_to_display(&video, settings.window_width, settings.window_height);
    let mut window = video
        .window(TITLE, w, h)
        .position_centered()
        .resizable()
        .high_pixel_density()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    let _ = window.set_minimum_size(320, 240);
    window.raise();
    if settings.fullscreen {
        let _ = window.set_fullscreen(true);
    }
    let mut canvas = window.into_canvas();
    let creator = canvas.texture_creator();
    let mut events = sdl
        .event_pump()
        .map_err(|e| format!("SDL events failed: {e}"))?;
    let event_subsystem = sdl.event().map_err(|e| format!("SDL events failed: {e}"))?;
    event_subsystem
        .register_custom_event::<FrameReady>()
        .map_err(|e| format!("SDL events failed: {e}"))?;

    let lang = Lang::system();
    let waker = || -> Wake {
        let sender = event_subsystem.event_sender();
        Box::new(move || {
            let _ = sender.push_custom_event(FrameReady);
        })
    };
    let mut screen = video::Screen::new(&creator, render::load_font(), settings.scaling);
    let mut pack_kind = PackKind::Missing;
    let mut mode = match app::startup(data_dir) {
        app::Startup::Setup(reason) => {
            log::log(&format!("setup: language {lang:?}"));
            enter_setup(reason, data_dir, lang, &mut screen)
        }
        app::Startup::Game(files) => {
            let (runner, kind) = start_game(
                args,
                data_dir,
                *files,
                &sdl,
                &event_subsystem,
                &mut screen,
                &settings,
            )?;
            pack_kind = kind;
            Mode::Game(runner)
        }
    };
    let mut ui = Ui {
        want_fullscreen: settings.fullscreen,
        lang,
        data_dir: data_dir.to_path_buf(),
        overlay: None,
        open_options: false,
        leave_game: None,
        pack_kind,
        settings,
        held: keys::HeldKeys::default(),
        quit: false,
        start_game: false,
        redraw: true,
        save_at: None,
        no_pack_logged: false,
    };
    let mut outcome: Result<(), String> = Ok(());
    // Asset errors of games shut down for a re-import or rebuild, reported at exit.
    let mut errs: Vec<String> = Vec::new();
    #[cfg(feature = "test-hooks")]
    let mut hook = testhook::TestHook::from_env();

    'main: loop {
        // Events first; a new frame arrives as a FrameReady event, so the wait can be long.
        // (Scripted test runs poll more often so script times stay precise.)
        let wait = if cfg!(feature = "test-hooks") {
            Duration::from_millis(10)
        } else {
            IDLE_WAIT
        };
        if let Some(ev) = events.wait_event_timeout(wait) {
            let mut pending = vec![ev];
            pending.extend(events.poll_iter());
            for ev in pending {
                ui.handle_event(ev, &mut canvas, &mut screen, &mut mode, &waker);
            }
        }
        // Setup: dialog results and import progress.
        if let Mode::Setup(s) = &mut mode {
            let ctx = Ctx {
                window: canvas.window(),
                waker: &waker,
            };
            ui.setup_outcome(s.poll(&ctx));
        }
        if ui.quit {
            break 'main;
        }
        // The options over the wizard's last pages.
        if std::mem::take(&mut ui.open_options)
            && let Mode::Setup(_) = &mode
            && ui.overlay.is_none()
        {
            ui.open_overlay(Place::Setup, None);
        }
        // Re-import or rebuild from the options in a game: the game ends (its playing
        // lock is released and the pack closed, so nothing holds the files the setup
        // replaces) and the wizard takes over in the same window.
        if let Some(reason) = ui.leave_game.take()
            && let Mode::Game(_) = &mode
        {
            log::log(&format!("options: leaving the game ({reason:?})"));
            let setup = Box::new(SetupMode::new(lang, reason.clone(), data_dir));
            if let Mode::Game(runner) = std::mem::replace(&mut mode, Mode::Setup(setup)) {
                errs.extend(runner.shutdown());
            }
            log::log(&reason.describe(data_dir));
            screen.set_overlay(None);
            screen.set_pack(None);
            ui.held = keys::HeldKeys::default();
        }
        // Setup finished (or asked to check again): the game if the data is usable now.
        if std::mem::take(&mut ui.start_game) {
            let files = match app::startup(data_dir) {
                app::Startup::Game(files) => Some(files),
                app::Startup::Setup(reason) => {
                    log::log(&reason.describe(data_dir));
                    if let Mode::Setup(s) = &mut mode {
                        s.restart(reason);
                    }
                    None
                }
            };
            if let Some(files) = files {
                match start_game(
                    args,
                    data_dir,
                    *files,
                    &sdl,
                    &event_subsystem,
                    &mut screen,
                    &ui.settings,
                ) {
                    Ok((runner, kind)) => {
                        ui.pack_kind = kind;
                        if ui.overlay.take().is_some() {
                            screen.set_overlay(None);
                        }
                        log::log("setup: done, starting the game");
                        if let Mode::Setup(s) = std::mem::replace(&mut mode, Mode::Game(runner)) {
                            s.shutdown();
                        }
                    }
                    Err(msg) => {
                        outcome = Err(msg);
                        break 'main;
                    }
                }
            }
        }
        if let Mode::Setup(s) = &mut mode
            && let Some(frame) = s.frame_if_dirty(screen.font())
        {
            screen.set_frame(frame, FrameSnapshot::default());
            ui.redraw = true;
        }
        let values = ui.values();
        if let Some(o) = &mut ui.overlay {
            o.set_values(values);
            if o.take_dirty() {
                screen.set_overlay(Some(o.frame(screen.font())));
                ui.redraw = true;
            }
        }
        #[cfg(feature = "test-hooks")]
        if let Some(h) = &mut hook {
            for r in h.tick(&event_subsystem, &mut canvas) {
                match r {
                    testhook::Request::ToggleFullscreen => ui.toggle_fullscreen(&mut canvas),
                }
            }
        }
        // Then the game's mailbox: the newest frame, and whether the engine has finished.
        if let Mode::Game(runner) = &mut mode {
            let (frame, end) = runner.poll();
            if let Some((frame, snap)) = frame {
                screen.set_frame(frame, snap);
                ui.redraw = true;
            }
            match end {
                None => {}
                Some(GameEnd::Finished(reason)) => {
                    if let Some(r) = reason {
                        log::log(&format!("game ended: {r}"));
                    }
                    break 'main;
                }
                Some(GameEnd::Crashed) => {
                    outcome = Err("the game thread stopped unexpectedly".into());
                    break 'main;
                }
                Some(GameEnd::QuitTimeout) => {
                    eprintln!("warning: the game did not stop in time; exiting anyway");
                    break 'main;
                }
            }
        }
        if ui.redraw {
            if let Err(e) = screen.draw(&mut canvas) {
                eprintln!("warning: draw failed: {e}");
            }
            #[cfg(feature = "test-hooks")]
            if let Some(h) = &mut hook {
                h.after_draw(&canvas, || {
                    capture_status(&canvas, &screen, &mode, ui.overlay.as_ref())
                });
            }
            canvas.present();
            ui.redraw = false;
        }
        if let Some(t) = ui.save_at
            && t.elapsed() >= SAVE_DELAY
        {
            ui.save_at = None;
            if let Err(e) = ui.settings.save(&settings_file) {
                eprintln!("warning: cannot save {}: {e}", settings_file.display());
            }
        }
    }

    // Normal exit: the game thread has reported done (or is gone); wait for it briefly.
    match mode {
        Mode::Game(runner) => {
            errs.extend(runner.shutdown());
            log::log(&format!(
                "art mode {} at exit: {}",
                screen.art().name(),
                screen.take_compose_stats().describe()
            ));
        }
        Mode::Setup(s) => s.shutdown(),
    }
    if let Err(e) = ui.settings.save(&settings_file) {
        eprintln!("warning: cannot save {}: {e}", settings_file.display());
    }
    if outcome.is_ok() && !errs.is_empty() && !headless() {
        let mut msg = format!(
            "{} asset problem(s) occurred; the game may have looked or sounded wrong. Details are in {}.\n\n",
            errs.len(),
            log::log_path(data_dir).display()
        );
        msg.push_str(&errs.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
        if errs.len() > 5 {
            msg.push_str("\n...");
        }
        if let Err(e) = show_simple_message_box(MessageBoxFlag::WARNING, TITLE, &msg, None) {
            eprintln!("(cannot show a message box: {e})");
        }
    }
    outcome
}

/// The status line logged with every test-hook capture.
#[cfg(feature = "test-hooks")]
fn capture_status(
    canvas: &sdl3::render::WindowCanvas,
    screen: &video::Screen,
    mode: &Mode,
    overlay: Option<&Overlay>,
) -> String {
    let (pw, ph) = canvas.window().size_in_pixels();
    let common = format!(
        "window={:?} pixels={pw}x{ph} fullscreen={:?} dest={:?} overlay={}",
        canvas.window().size(),
        canvas.window().fullscreen_state(),
        screen.dest(),
        match overlay {
            None => "none",
            Some(o) if o.confirming() => "confirm",
            Some(_) => "open",
        }
    );
    match mode {
        Mode::Setup(s) => format!(
            "mode=setup reason={:?} page={:?} lang={:?} {common}",
            s.wizard().reason(),
            s.wizard().page(),
            s.wizard().lang()
        ),
        Mode::Game(runner) => {
            let m = lock(runner.mixer());
            let (lru, stream) = screen.pack_memory().unwrap_or_default();
            format!(
                "mode=game art={} compose_ms={:.2} pack_lru={lru} pack_stream={stream} {common} voice_busy={} sfx={} audio_frames={} peak={:.3}",
                screen.art().name(),
                screen.last_compose().as_secs_f64() * 1000.0,
                m.voice_busy(),
                m.sfx_count(),
                runner
                    .audio_stats()
                    .frames
                    .load(std::sync::atomic::Ordering::Relaxed),
                runner.audio_stats().take_peak()
            )
        }
    }
}

/// Shrink the saved window size to the usable display area, keeping 4:3.
fn fit_to_display(video: &sdl3::VideoSubsystem, w: u32, h: u32) -> (u32, u32) {
    let Ok(bounds) = video
        .get_primary_display()
        .and_then(|d| d.get_usable_bounds())
    else {
        return (w, h);
    };
    let (bw, bh) = (bounds.width() * 9 / 10, bounds.height() * 9 / 10);
    if w <= bw && h <= bh {
        return (w, h);
    }
    let k = f64::min(f64::from(bw) / f64::from(w), f64::from(bh) / f64::from(h));
    (
        ((f64::from(w) * k) as u32).max(320),
        ((f64::from(h) * k) as u32).max(240),
    )
}

/// Main-loop state touched by event handling.
struct Ui {
    settings: Settings,
    lang: Lang,
    data_dir: PathBuf,
    /// The options overlay, when open (the game is paused meanwhile).
    overlay: Option<Overlay>,
    /// The wizard asked for the options.
    open_options: bool,
    /// The options asked to end the game for a re-import or rebuild.
    leave_game: Option<SetupReason>,
    /// What the running game found in `upscaled/`.
    pack_kind: PackKind,
    /// Fullscreen as last requested. The window state follows asynchronously
    /// (Enter/LeaveFullscreen events update `settings.fullscreen`), so toggles flip this
    /// rather than reading the window.
    want_fullscreen: bool,
    /// Keys we reported down, by physical key; released on focus loss.
    held: keys::HeldKeys,
    /// Leave the main loop now (setup mode has nothing to unwind).
    quit: bool,
    /// Setup is done: check the data dir and start the game.
    start_game: bool,
    redraw: bool,
    /// Settings changed at this time and not yet saved.
    save_at: Option<Instant>,
    /// F8 without a pack was logged.
    no_pack_logged: bool,
}

impl Ui {
    fn setup_outcome(&mut self, out: Outcome) {
        match out {
            Outcome::None => {}
            Outcome::Quit => self.quit = true,
            Outcome::StartGame => self.start_game = true,
            Outcome::OpenOptions => self.open_options = true,
        }
    }

    /// The settings the options show.
    fn values(&self) -> options::Values {
        options::Values {
            art: self.settings.art_mode,
            scaling: self.settings.scaling,
            fullscreen: self.want_fullscreen,
            volume: self.settings.volume,
        }
    }

    /// Opens the options. Over a game: the held keys are released to the game and the
    /// game and its sound pause (see `host.rs`).
    fn open_overlay(&mut self, place: Place, runner: Option<&GameRunner>) {
        let pack = match place {
            Place::Game => self.pack_kind,
            Place::Setup => art::open_pack(&self.data_dir).kind(),
        };
        if let Some(runner) = runner {
            let mut input = lock(&runner.shared().input);
            for vk in self.held.release_all() {
                input.key_up(vk);
            }
            drop(input);
            runner.set_paused(true);
        }
        log::log(&format!("options: open ({place:?}, pack {pack:?})"));
        self.overlay = Some(Overlay::new(self.lang, place, pack, self.values()));
    }

    fn close_overlay(&mut self, screen: &mut video::Screen, mode: &mut Mode) {
        if self.overlay.take().is_none() {
            return;
        }
        log::log("options: closed");
        screen.set_overlay(None);
        self.redraw = true;
        match mode {
            Mode::Game(runner) => runner.set_paused(false),
            Mode::Setup(s) => s.touch(),
        }
    }

    /// Input while the options are open: keys and the mouse go to the overlay. False for
    /// the common handling (fullscreen toggle, window events, quit).
    fn overlay_event(
        &mut self,
        ev: &Event,
        canvas: &mut sdl3::render::WindowCanvas,
        screen: &mut video::Screen,
        mode: &mut Mode,
    ) -> bool {
        let Some(o) = &mut self.overlay else {
            return false;
        };
        let effect = match *ev {
            Event::KeyDown { .. } => match overlay_key(ev) {
                Some(OverlayKey::Key(k)) => o.key(k),
                Some(OverlayKey::PassThrough) => return false,
                Some(OverlayKey::Swallow) | None => return true,
            },
            Event::MouseMotion { x, y, .. } => {
                let (gx, gy) = to_game(canvas, screen, x, y);
                o.pointer(gx, gy);
                options::Effect::None
            }
            Event::MouseButtonDown {
                mouse_btn: sdl3::mouse::MouseButton::Left,
                x,
                y,
                ..
            } => {
                let (gx, gy) = to_game(canvas, screen, x, y);
                o.click(gx, gy)
            }
            Event::MouseButtonDown { .. } | Event::DropFile { .. } => options::Effect::None,
            _ => return false,
        };
        self.apply_option(effect, canvas, screen, mode);
        true
    }

    fn apply_option(
        &mut self,
        effect: options::Effect,
        canvas: &mut sdl3::render::WindowCanvas,
        screen: &mut video::Screen,
        mode: &mut Mode,
    ) {
        use options::Effect as E;
        match effect {
            E::None => {}
            E::Art(art) => {
                self.settings.art_mode = art;
                self.save_at = Some(Instant::now());
                if let Mode::Game(_) = mode {
                    let stats = screen.take_compose_stats();
                    screen.set_art(art);
                    log::log(&format!(
                        "art mode: {} (options; before: {})",
                        screen.art().name(),
                        stats.describe()
                    ));
                }
            }
            E::Scaling(m) => {
                self.settings.scaling = m;
                screen.set_scaling(m);
                self.save_at = Some(Instant::now());
                self.redraw = true;
            }
            E::Fullscreen(on) => {
                if on != self.want_fullscreen {
                    self.toggle_fullscreen(canvas);
                }
            }
            E::Volume(v) => {
                self.settings.volume = v;
                self.save_at = Some(Instant::now());
                if let Mode::Game(runner) = mode {
                    runner.set_volume(v);
                }
            }
            E::Close => self.close_overlay(screen, mode),
            E::Reimport | E::Rebuild => {
                let reimport = effect == E::Reimport;
                if let Mode::Game(_) = mode {
                    // The main loop shuts the game down; it stays paused until then.
                    self.overlay = None;
                    self.leave_game = Some(if reimport {
                        SetupReason::Reimport
                    } else {
                        SetupReason::Rebuild
                    });
                } else {
                    self.close_overlay(screen, mode);
                    if let Mode::Setup(s) = mode {
                        if reimport {
                            s.reimport();
                        } else {
                            s.rebuild();
                        }
                    }
                }
            }
        }
    }

    /// Wizard input: navigation keys, the mouse and dropped files. False when the event
    /// is for the common handling (fullscreen toggle, window events, quit).
    fn setup_event(
        &mut self,
        ev: &Event,
        canvas: &sdl3::render::WindowCanvas,
        screen: &video::Screen,
        setup: &mut SetupMode,
        waker: &dyn Fn() -> Wake,
    ) -> bool {
        let ctx = Ctx {
            window: canvas.window(),
            waker,
        };
        let out = match *ev {
            Event::KeyDown {
                keycode: Some(key),
                scancode,
                keymod,
                repeat,
                ..
            } => {
                let vk = keys::vk_for(Some(key), scancode);
                let alt = keymod.intersects(Mod::LALTMOD | Mod::RALTMOD);
                let ctrl = keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
                let gui = keymod.intersects(Mod::LGUIMOD | Mod::RGUIMOD);
                if keys::is_fullscreen_toggle(vk, alt, ctrl, gui) {
                    return false;
                }
                let shift = keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
                let nav = match key {
                    Keycode::Up | Keycode::Left => Nav::Prev,
                    Keycode::Down | Keycode::Right => Nav::Next,
                    Keycode::Tab if shift => Nav::Prev,
                    Keycode::Tab => Nav::Next,
                    Keycode::Return | Keycode::KpEnter | Keycode::Space if !repeat => Nav::Activate,
                    Keycode::Escape if !repeat => Nav::Back,
                    _ => return true,
                };
                setup.key(nav, &ctx)
            }
            Event::MouseMotion { x, y, .. } => {
                let (gx, gy) = to_game(canvas, screen, x, y);
                setup.pointer(gx, gy);
                Outcome::None
            }
            Event::MouseButtonDown {
                mouse_btn: sdl3::mouse::MouseButton::Left,
                x,
                y,
                ..
            } => {
                let (gx, gy) = to_game(canvas, screen, x, y);
                setup.click(gx, gy, &ctx)
            }
            Event::DropFile { ref filename, .. } => setup.dropped(Path::new(filename), &ctx),
            _ => return false,
        };
        self.setup_outcome(out);
        true
    }

    fn handle_event(
        &mut self,
        ev: Event,
        canvas: &mut sdl3::render::WindowCanvas,
        screen: &mut video::Screen,
        mode: &mut Mode,
        waker: &dyn Fn() -> Wake,
    ) {
        if self.overlay.is_some() && self.overlay_event(&ev, canvas, screen, mode) {
            return;
        }
        if let Mode::Setup(setup) = &mut *mode
            && self.overlay.is_none()
            && self.setup_event(&ev, canvas, screen, setup, waker)
        {
            return;
        }
        // Game input goes to the game thread; setup input is handled above.
        let shared = match mode {
            Mode::Game(runner) => Some(runner.shared()),
            Mode::Setup(_) => None,
        };
        match ev {
            // Only a wakeup; frees the (empty) payload SDL carried for us.
            ref e if e.is_user_event() => {
                let _ = e.as_user_event_type::<FrameReady>();
            }
            Event::Quit { .. }
            | Event::Window {
                win_event: WindowEvent::CloseRequested,
                ..
            } => match mode {
                Mode::Game(runner) => runner.request_quit(),
                Mode::Setup(_) => self.quit = true,
            },
            Event::KeyDown {
                keycode,
                scancode,
                keymod,
                repeat,
                ..
            } => {
                let vk = keys::vk_for(keycode, scancode);
                let alt = keymod.intersects(Mod::LALTMOD | Mod::RALTMOD);
                let ctrl = keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
                let gui = keymod.intersects(Mod::LGUIMOD | Mod::RGUIMOD);
                if keys::is_fullscreen_toggle(vk, alt, ctrl, gui) {
                    if !repeat {
                        self.toggle_fullscreen(canvas);
                    }
                    return;
                }
                // F10 (the port's own key) opens the options over the paused game.
                if keycode == Some(Keycode::F10) {
                    if !repeat
                        && self.overlay.is_none()
                        && let Mode::Game(runner) = &*mode
                    {
                        self.open_overlay(Place::Game, Some(runner));
                        self.redraw = true;
                    }
                    return;
                }
                let Some(shared) = shared else { return };
                // F8 is the port's own key (the game never reads it).
                if keycode == Some(Keycode::F8) {
                    if !repeat {
                        self.toggle_art(screen);
                    }
                    return;
                }
                if let Some(vk) = vk {
                    self.held.press(scancode.unwrap_or(Scancode::Unknown), vk);
                    lock(&shared.input).key_down(vk);
                }
            }
            Event::KeyUp { scancode, .. } => {
                if let Some(vk) = self.held.release(scancode.unwrap_or(Scancode::Unknown))
                    && let Some(shared) = shared
                {
                    lock(&shared.input).key_up(vk);
                }
            }
            Event::MouseMotion { x, y, .. } => {
                if let Some(shared) = shared {
                    let (gx, gy) = to_game(canvas, screen, x, y);
                    lock(&shared.input).mouse_move(gx, gy);
                }
            }
            Event::MouseButtonDown {
                mouse_btn, x, y, ..
            } => {
                let Some(shared) = shared else { return };
                let (gx, gy) = to_game(canvas, screen, x, y);
                let mut input = lock(&shared.input);
                input.mouse_move(gx, gy);
                match mouse_btn {
                    sdl3::mouse::MouseButton::Left => input.mouse_button(MouseButton::Left),
                    sdl3::mouse::MouseButton::Right => input.mouse_button(MouseButton::Right),
                    _ => {}
                }
            }
            Event::Window { win_event, .. } => match win_event {
                WindowEvent::Resized(w, h) => {
                    if canvas.window().fullscreen_state() == FullscreenType::Off && w > 0 && h > 0 {
                        self.settings.window_width = w as u32;
                        self.settings.window_height = h as u32;
                        self.save_at = Some(Instant::now());
                    }
                    self.redraw = true;
                }
                WindowEvent::PixelSizeChanged(..)
                | WindowEvent::Exposed
                | WindowEvent::Shown
                | WindowEvent::Restored
                | WindowEvent::Maximized
                | WindowEvent::DisplayChanged(_) => self.redraw = true,
                WindowEvent::EnterFullscreen => {
                    #[cfg(feature = "test-hooks")]
                    eprintln!("event: EnterFullscreen");
                    self.settings.fullscreen = true;
                    self.save_at = Some(Instant::now());
                    self.redraw = true;
                }
                WindowEvent::LeaveFullscreen => {
                    #[cfg(feature = "test-hooks")]
                    eprintln!("event: LeaveFullscreen");
                    self.settings.fullscreen = false;
                    self.save_at = Some(Instant::now());
                    self.redraw = true;
                }
                WindowEvent::FocusLost => {
                    // Keys released while unfocused never reach us; do not leave them stuck.
                    let released = self.held.release_all();
                    if let Some(shared) = shared {
                        let mut input = lock(&shared.input);
                        for vk in released {
                            input.key_up(vk);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// F8: original/upscaled art from the next compose on; saved like other settings.
    fn toggle_art(&mut self, screen: &mut video::Screen) {
        match art::f8_toggle(screen.art(), screen.has_pack()) {
            art::F8::Switched(mode) => {
                let stats = screen.take_compose_stats();
                let (lru, stream) = screen.pack_memory().unwrap_or_default();
                // Switching to the original art frees the decoded upscaled frames.
                screen.set_art(mode);
                self.settings.art_mode = mode;
                self.save_at = Some(Instant::now());
                self.redraw = true;
                log::log(&format!(
                    "art mode: {} (F8; before: {}; decoded art held {} MB + {} MB stream)",
                    mode.name(),
                    stats.describe(),
                    lru >> 20,
                    stream >> 20
                ));
            }
            art::F8::NoPack => {
                if !self.no_pack_logged {
                    self.no_pack_logged = true;
                    log::log(
                        "art mode: F8 ignored, no valid upscaled pack (see the start of the log)",
                    );
                }
            }
        }
    }

    fn toggle_fullscreen(&mut self, canvas: &mut sdl3::render::WindowCanvas) {
        let on = !self.want_fullscreen;
        match canvas.window_mut().set_fullscreen(on) {
            Ok(()) => self.want_fullscreen = on,
            Err(e) => eprintln!("warning: cannot toggle fullscreen: {e}"),
        }
        self.redraw = true;
    }
}

/// What a key press means while the options overlay is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayKey {
    /// For the overlay.
    Key(options::Key),
    /// The fullscreen toggle: the common handling.
    PassThrough,
    /// Anything else, a key without a keycode included: dropped, so nothing reaches the
    /// paused game.
    Swallow,
}

/// `None` for an event that is not a key press.
fn overlay_key(ev: &Event) -> Option<OverlayKey> {
    let Event::KeyDown {
        keycode,
        scancode,
        keymod,
        repeat,
        ..
    } = *ev
    else {
        return None;
    };
    let vk = keys::vk_for(keycode, scancode);
    let alt = keymod.intersects(Mod::LALTMOD | Mod::RALTMOD);
    let ctrl = keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
    let gui = keymod.intersects(Mod::LGUIMOD | Mod::RGUIMOD);
    if keys::is_fullscreen_toggle(vk, alt, ctrl, gui) {
        return Some(OverlayKey::PassThrough);
    }
    let shift = keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
    let k = match keycode {
        Some(Keycode::Up) => options::Key::Up,
        Some(Keycode::Down) => options::Key::Down,
        Some(Keycode::Tab) if shift => options::Key::Up,
        Some(Keycode::Tab) => options::Key::Down,
        Some(Keycode::Left) => options::Key::Left,
        Some(Keycode::Right) => options::Key::Right,
        Some(Keycode::Return | Keycode::KpEnter | Keycode::Space) if !repeat => {
            options::Key::Activate
        }
        Some(Keycode::Escape | Keycode::F10) if !repeat => options::Key::Back,
        _ => return Some(OverlayKey::Swallow),
    };
    Some(OverlayKey::Key(k))
}

/// Window coordinates (points) of a mouse event to 640x480, HiDPI-correct: scale by the
/// window's pixel/point ratio, then invert the letterbox transform.
fn to_game(
    canvas: &sdl3::render::WindowCanvas,
    screen: &video::Screen,
    x: f32,
    y: f32,
) -> (i32, i32) {
    let window = canvas.window();
    let (ww, wh) = window.size();
    let (pw, ph) = window.size_in_pixels();
    let sx = if ww > 0 { pw as f32 / ww as f32 } else { 1.0 };
    let sy = if wh > 0 { ph as f32 / wh as f32 } else { 1.0 };
    layout::map_mouse(screen.dest(), x * sx, y * sy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_down(keycode: Option<Keycode>, scancode: Option<Scancode>, keymod: Mod) -> Event {
        Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode,
            scancode,
            keymod,
            repeat: false,
            which: 0,
            raw: 0,
        }
    }

    /// While the options are open no key press reaches the paused game: one without a
    /// keycode is swallowed too; only the fullscreen toggle passes through.
    #[test]
    fn overlay_swallows_every_key() {
        let k = |kc, sc, m| overlay_key(&key_down(kc, sc, m));
        assert_eq!(
            k(None, Some(Scancode::A), Mod::NOMOD),
            Some(OverlayKey::Swallow)
        );
        assert_eq!(k(None, None, Mod::NOMOD), Some(OverlayKey::Swallow));
        assert_eq!(
            k(Some(Keycode::A), Some(Scancode::A), Mod::NOMOD),
            Some(OverlayKey::Swallow)
        );
        assert_eq!(
            k(Some(Keycode::Up), Some(Scancode::Up), Mod::NOMOD),
            Some(OverlayKey::Key(options::Key::Up))
        );
        assert_eq!(
            k(Some(Keycode::Tab), Some(Scancode::Tab), Mod::LSHIFTMOD),
            Some(OverlayKey::Key(options::Key::Up))
        );
        assert_eq!(
            k(Some(Keycode::F10), Some(Scancode::F10), Mod::NOMOD),
            Some(OverlayKey::Key(options::Key::Back))
        );
        assert_eq!(
            k(Some(Keycode::F11), Some(Scancode::F11), Mod::NOMOD),
            Some(OverlayKey::PassThrough)
        );
        assert_eq!(overlay_key(&Event::Quit { timestamp: 0 }), None);
    }
}
