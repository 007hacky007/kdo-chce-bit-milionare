//! The setup wizard: guides the player through importing their own disc in the game
//! window, before any game data exists, then offers to make the enhanced graphics.
//!
//! [`Wizard`] is the pure state machine (pages, focus, transitions); it never touches
//! SDL. [`SetupMode`] runs it in the app: file dialogs and dropped files feed it paths,
//! an [`ImportJob`] runs `setup::import` and an [`UpscaleJob`] the upscale pipeline on
//! worker threads with progress and cancel, and [`screens`] draws the pages as display
//! lists. The options overlay can also come back here from a game (re-import, rebuild
//! the enhanced graphics): the game has been shut down by then.

mod screens;
mod upscale;
pub(crate) mod widgets;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use render::{Font, Frame};
use setup::{ImportError, ImportReport, Progress, Source};

use ::upscale as upscale_crate;

use crate::app::SetupReason;
use crate::art::PackKind;
use crate::host::{Wake, lock};
use crate::strings::{Lang, Str, text};

pub use screens::ErrorText;
pub use upscale::{RunnerSource, Status as UpscaleStatus, UpscaleFail, UpscaleJob};

/// Keyboard navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Prev,
    Next,
    Activate,
    /// Esc: back, or cancel the import.
    Back,
}

/// What a button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue,
    Quit,
    PickFile,
    PickFolder,
    Back,
    Cancel,
    /// Run the failed import again.
    Retry,
    /// Check the data dir again (after an import elsewhere).
    RetryStartup,
    OtherSource,
    StartGame,
    Options,
    RunUpscale,
    /// Run on a CPU-only device after the warning.
    RunAnyway,
    /// Play with the original art (starts the game).
    SkipUpscale,
    CancelUpscale,
    /// Leave a re-import or rebuild started from the game: start it again.
    BackToGame,
}

/// What the app has to do after a wizard transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    PickFile,
    PickFolder,
    StartImport(Source),
    CancelImport,
    StartGame,
    RetryStartup,
    StartUpscale {
        /// Go on even if the Vulkan device is CPU-only.
        allow_cpu: bool,
    },
    CancelUpscale,
    OpenOptions,
}

/// A notice shown on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    NotRecognized(PathBuf),
    DialogFailed(String),
    Cancelled,
    StillRunning,
}

/// The pages, without their data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    Welcome,
    Waiting,
    Choose,
    Importing,
    Failed,
    Done,
    /// The offer to make the enhanced graphics.
    Upscale,
    Upscaling,
    UpscaleCpu,
    UpscaleFailed,
    UpscaleDone,
}

/// Why the upscale offer is shown, and where Esc leads from it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UpCtx {
    /// Started from the options in a game (Esc and skip lead back to the game).
    rebuild: bool,
    /// The import just done (Esc goes back to its result).
    report: Option<ImportReport>,
    /// The pack found after the import.
    pack: PackKind,
}

#[derive(Debug, Clone)]
enum Page {
    Welcome,
    Waiting,
    Choose,
    Importing {
        source: Source,
        progress: Option<Progress>,
        cancelling: bool,
    },
    Failed {
        source: Source,
        text: ErrorText,
    },
    Done {
        report: ImportReport,
        pack: PackKind,
    },
    Upscale {
        ctx: UpCtx,
        cancelled: bool,
    },
    Upscaling {
        ctx: UpCtx,
        status: Option<UpscaleStatus>,
        cancelling: bool,
    },
    UpscaleCpu {
        ctx: UpCtx,
        device: String,
    },
    UpscaleFailed {
        ctx: UpCtx,
        text: ErrorText,
    },
    UpscaleDone {
        report: upscale_crate::Report,
    },
}

/// The wizard's state.
#[derive(Debug, Clone)]
pub struct Wizard {
    lang: Lang,
    reason: SetupReason,
    page: Page,
    focus: usize,
    note: Option<Note>,
    dirty: bool,
    /// A game was ended to get here (re-import or rebuild from its options): the way
    /// out says "back to the game". Over the wizard's own last pages it starts the game.
    from_game: bool,
}

impl Wizard {
    /// `Reimport` and `Rebuild` mean a game was left for them.
    pub fn new(lang: Lang, reason: SetupReason) -> Wizard {
        let page = Self::first_page(&reason);
        let from_game = matches!(reason, SetupReason::Reimport | SetupReason::Rebuild);
        Wizard {
            lang,
            reason,
            page,
            focus: 0,
            note: None,
            dirty: true,
            from_game,
        }
    }

    /// The way out of a re-import or rebuild: back to the game that was left, or start
    /// it (with the pack it has), or play with the original graphics.
    fn leave(&self, pack: PackKind) -> Action {
        if self.from_game {
            Action::BackToGame
        } else if pack == PackKind::Ready {
            Action::StartGame
        } else {
            Action::SkipUpscale
        }
    }

    /// Esc leads to the game here (the hint says so).
    pub(crate) fn esc_starts_game(&self) -> bool {
        match &self.page {
            Page::Choose => self.reason == SetupReason::Reimport,
            Page::Upscale { ctx, .. } => ctx.rebuild,
            _ => false,
        }
    }

    pub(crate) fn opened_from_game(&self) -> bool {
        self.from_game
    }

    fn first_page(reason: &SetupReason) -> Page {
        match reason {
            SetupReason::ImportRunning => Page::Waiting,
            SetupReason::NoImport | SetupReason::Unreadable(_) => Page::Welcome,
            SetupReason::Reimport => Page::Choose,
            SetupReason::Rebuild => Page::Upscale {
                ctx: UpCtx {
                    rebuild: true,
                    report: None,
                    pack: PackKind::Missing,
                },
                cancelled: false,
            },
        }
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn reason(&self) -> &SetupReason {
        &self.reason
    }

    pub fn page(&self) -> PageKind {
        match self.page {
            Page::Welcome => PageKind::Welcome,
            Page::Waiting => PageKind::Waiting,
            Page::Choose => PageKind::Choose,
            Page::Importing { .. } => PageKind::Importing,
            Page::Failed { .. } => PageKind::Failed,
            Page::Done { .. } => PageKind::Done,
            Page::Upscale { .. } => PageKind::Upscale,
            Page::Upscaling { .. } => PageKind::Upscaling,
            Page::UpscaleCpu { .. } => PageKind::UpscaleCpu,
            Page::UpscaleFailed { .. } => PageKind::UpscaleFailed,
            Page::UpscaleDone { .. } => PageKind::UpscaleDone,
        }
    }

    #[cfg(test)]
    pub fn focus(&self) -> usize {
        self.focus
    }

    #[cfg(test)]
    pub fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }

    #[cfg(test)]
    pub fn cancelling(&self) -> bool {
        matches!(
            self.page,
            Page::Importing {
                cancelling: true,
                ..
            }
        )
    }

    /// The buttons of the current page, top to bottom.
    pub fn actions(&self) -> Vec<Action> {
        match &self.page {
            Page::Welcome => vec![Action::Continue, Action::Quit],
            Page::Waiting => vec![Action::RetryStartup, Action::Quit],
            Page::Choose if self.reason == SetupReason::Reimport => {
                let leave = if self.from_game {
                    Action::BackToGame
                } else {
                    Action::StartGame
                };
                vec![Action::PickFile, Action::PickFolder, leave]
            }
            Page::Choose => vec![Action::PickFile, Action::PickFolder, Action::Back],
            Page::Importing { .. } => vec![Action::Cancel],
            Page::Failed { text, .. } if text.retry => {
                vec![Action::Retry, Action::OtherSource, Action::Quit]
            }
            Page::Failed { .. } => vec![Action::OtherSource, Action::Quit],
            Page::Done {
                pack: PackKind::Ready,
                ..
            }
            | Page::UpscaleDone { .. } => vec![Action::StartGame, Action::Options],
            Page::Done { .. } => vec![Action::Continue, Action::Options],
            Page::Upscale { ctx, .. } if ctx.rebuild => {
                vec![Action::RunUpscale, self.leave(ctx.pack)]
            }
            Page::Upscale { .. } => vec![Action::RunUpscale, Action::SkipUpscale],
            Page::Upscaling { .. } => vec![Action::CancelUpscale],
            // Started from a game: leave back to it (with whatever pack it had).
            Page::UpscaleCpu { ctx, .. } if ctx.rebuild => {
                vec![Action::RunAnyway, self.leave(ctx.pack)]
            }
            Page::UpscaleCpu { .. } => vec![Action::RunAnyway, Action::SkipUpscale],
            Page::UpscaleFailed { text, ctx } => {
                let leave = if ctx.rebuild {
                    self.leave(ctx.pack)
                } else {
                    Action::SkipUpscale
                };
                if text.retry {
                    vec![Action::Retry, leave]
                } else {
                    vec![leave]
                }
            }
        }
    }

    /// The label of a button.
    pub fn label(&self, action: Action) -> &'static str {
        text(
            self.lang,
            match action {
                Action::Continue => Str::BtnContinue,
                Action::Quit => Str::BtnQuit,
                Action::PickFile => Str::BtnPickFile,
                Action::PickFolder => Str::BtnPickFolder,
                Action::Back => Str::BtnBack,
                Action::Cancel => Str::BtnCancel,
                Action::Retry | Action::RetryStartup => Str::BtnRetry,
                Action::OtherSource => Str::BtnOtherSource,
                Action::StartGame => Str::BtnStartGame,
                Action::Options => Str::BtnOptions,
                Action::RunUpscale => Str::BtnRunUpscale,
                Action::RunAnyway => Str::BtnRunAnyway,
                Action::SkipUpscale => Str::BtnSkipUpscale,
                Action::CancelUpscale => Str::BtnCancelUpscale,
                Action::BackToGame => Str::BtnBackToGame,
            },
        )
    }

    /// The page needs drawing again (cleared by the call).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        self.focus = 0;
        self.dirty = true;
    }

    pub fn key(&mut self, nav: Nav) -> Effect {
        let n = self.actions().len();
        self.dirty = true;
        match nav {
            Nav::Next if n > 0 => self.focus = (self.focus + 1) % n,
            Nav::Prev if n > 0 => self.focus = (self.focus + n - 1) % n,
            Nav::Next | Nav::Prev => {}
            Nav::Activate => {
                if let Some(&a) = self.actions().get(self.focus) {
                    return self.activate(a);
                }
            }
            Nav::Back => return self.back(),
        }
        Effect::None
    }

    /// The pointer moved to (x, y) in 640x480; true when the focus changed.
    pub fn pointer(&mut self, x: i32, y: i32) -> bool {
        match widgets::hit(&widgets::button_rects(self.actions().len()), x, y) {
            Some(i) if i != self.focus => {
                self.focus = i;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// A left click at (x, y) in 640x480.
    pub fn click(&mut self, x: i32, y: i32) -> Effect {
        let actions = self.actions();
        match widgets::hit(&widgets::button_rects(actions.len()), x, y) {
            Some(i) => {
                self.focus = i;
                self.dirty = true;
                self.activate(actions[i])
            }
            None => Effect::None,
        }
    }

    fn activate(&mut self, action: Action) -> Effect {
        match action {
            Action::Continue => {
                match std::mem::replace(&mut self.page, Page::Welcome) {
                    Page::Done { report, pack } => self.go(Page::Upscale {
                        ctx: UpCtx {
                            rebuild: false,
                            report: Some(report),
                            pack,
                        },
                        cancelled: false,
                    }),
                    Page::Welcome => self.go(Page::Choose),
                    other => self.page = other,
                }
                Effect::None
            }
            Action::Options => Effect::OpenOptions,
            Action::RunUpscale => self.start_upscale(false),
            Action::RunAnyway => self.start_upscale(true),
            Action::SkipUpscale => Effect::StartGame,
            Action::BackToGame => Effect::RetryStartup,
            Action::CancelUpscale => self.cancel(),
            Action::Quit => Effect::Quit,
            Action::PickFile => Effect::PickFile,
            Action::PickFolder => Effect::PickFolder,
            Action::Back => self.back(),
            Action::Cancel => self.cancel(),
            Action::Retry => match &self.page {
                Page::Failed { source, .. } => {
                    let source = source.clone();
                    self.start(source)
                }
                Page::UpscaleFailed { .. } => self.start_upscale(false),
                _ => Effect::None,
            },
            Action::RetryStartup => Effect::RetryStartup,
            Action::OtherSource => {
                self.note = None;
                self.go(Page::Choose);
                Effect::None
            }
            Action::StartGame => Effect::StartGame,
        }
    }

    fn back(&mut self) -> Effect {
        match &self.page {
            Page::Welcome | Page::Waiting => Effect::Quit,
            // A re-import from the game: the old data is still there.
            Page::Choose if self.reason == SetupReason::Reimport => Effect::RetryStartup,
            Page::Choose => {
                self.note = None;
                self.go(Page::Welcome);
                Effect::None
            }
            Page::Importing { .. } => self.cancel(),
            Page::Failed { .. } => {
                self.note = None;
                self.go(Page::Choose);
                Effect::None
            }
            Page::Done { .. } | Page::UpscaleDone { .. } => Effect::None,
            Page::Upscale { ctx, .. } => match &ctx.report {
                _ if ctx.rebuild => Effect::RetryStartup,
                Some(report) => {
                    let (report, pack) = (report.clone(), ctx.pack);
                    self.go(Page::Done { report, pack });
                    Effect::None
                }
                None => Effect::None,
            },
            Page::Upscaling { .. } => self.cancel(),
            Page::UpscaleCpu { ctx, .. } | Page::UpscaleFailed { ctx, .. } => {
                let ctx = ctx.clone();
                self.go(Page::Upscale {
                    ctx,
                    cancelled: false,
                });
                Effect::None
            }
        }
    }

    fn cancel(&mut self) -> Effect {
        match &mut self.page {
            Page::Importing { cancelling, .. } if !*cancelling => {
                *cancelling = true;
                self.dirty = true;
                Effect::CancelImport
            }
            Page::Upscaling { cancelling, .. } if !*cancelling => {
                *cancelling = true;
                self.dirty = true;
                Effect::CancelUpscale
            }
            _ => Effect::None,
        }
    }

    fn up_ctx(&self) -> Option<UpCtx> {
        match &self.page {
            Page::Upscale { ctx, .. }
            | Page::UpscaleCpu { ctx, .. }
            | Page::UpscaleFailed { ctx, .. }
            | Page::Upscaling { ctx, .. } => Some(ctx.clone()),
            _ => None,
        }
    }

    fn start_upscale(&mut self, allow_cpu: bool) -> Effect {
        let Some(ctx) = self.up_ctx() else {
            return Effect::None;
        };
        self.go(Page::Upscaling {
            ctx,
            status: None,
            cancelling: false,
        });
        Effect::StartUpscale { allow_cpu }
    }

    /// Upscale progress; true when it changed what is shown.
    pub fn upscale_status(&mut self, st: UpscaleStatus) -> bool {
        match &mut self.page {
            Page::Upscaling { status, .. } if status.as_ref() != Some(&st) => {
                *status = Some(st);
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// The upscale step ended.
    pub fn upscale_finished(&mut self, result: Result<upscale_crate::Report, UpscaleFail>) {
        let Page::Upscaling { ctx, .. } = &self.page else {
            return;
        };
        let ctx = ctx.clone();
        match result {
            Ok(report) => self.go(Page::UpscaleDone { report }),
            Err(UpscaleFail::CpuOnly(device)) => self.go(Page::UpscaleCpu { ctx, device }),
            Err(UpscaleFail::Error(upscale_crate::UpscaleError::Cancelled)) => {
                self.go(Page::Upscale {
                    ctx,
                    cancelled: true,
                })
            }
            Err(f) => {
                let text = upscale::fail_text(self.lang, &f);
                self.go(Page::UpscaleFailed { ctx, text });
            }
        }
    }

    /// The options asked to import again (from the wizard's last pages): the data is
    /// there, so Esc on the source page leads to the game.
    pub fn reimport(&mut self) {
        self.from_game = false;
        self.reason = SetupReason::Reimport;
        self.note = None;
        self.go(Page::Choose);
    }

    /// The options asked to make the enhanced graphics again; `pack` is what is in
    /// `upscaled/` now (the offer explains a missing or stale pack differently).
    pub fn rebuild(&mut self, pack: PackKind) {
        self.from_game = false;
        self.reason = SetupReason::Rebuild;
        self.note = None;
        let page = Self::first_page(&self.reason);
        self.go(page);
        self.set_offer_pack(pack);
    }

    /// What is in `upscaled/`, for the offer's text.
    pub fn set_offer_pack(&mut self, pack: PackKind) {
        if let Page::Upscale { ctx, .. } = &mut self.page {
            ctx.pack = pack;
            self.dirty = true;
        }
    }

    fn start(&mut self, source: Source) -> Effect {
        self.note = None;
        self.go(Page::Importing {
            source: source.clone(),
            progress: None,
            cancelling: false,
        });
        Effect::StartImport(source)
    }

    /// A path from a file dialog or dropped onto the window.
    pub fn choose(&mut self, path: &Path) -> Effect {
        if !matches!(
            self.page,
            Page::Welcome | Page::Choose | Page::Failed { .. }
        ) {
            return Effect::None;
        }
        match setup::detect_source(path) {
            Ok(source) => self.start(source),
            Err(_) => {
                self.note = Some(Note::NotRecognized(path.to_path_buf()));
                self.go(Page::Choose);
                Effect::None
            }
        }
    }

    /// The file dialog could not be shown.
    pub fn dialog_failed(&mut self, reason: String) {
        if matches!(
            self.page,
            Page::Welcome | Page::Choose | Page::Failed { .. }
        ) {
            self.note = Some(Note::DialogFailed(reason));
            self.go(Page::Choose);
        }
    }

    /// Import progress; true when it changed what is shown.
    pub fn progress(&mut self, p: Progress) -> bool {
        match &mut self.page {
            Page::Importing { progress, .. } if *progress != Some(p) => {
                *progress = Some(p);
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// The import ended. `pack`: what is in `upscaled/` for the new data.
    pub fn finished(&mut self, result: Result<ImportReport, ImportError>, pack: PackKind) {
        let Page::Importing { source, .. } = &self.page else {
            return;
        };
        let source = source.clone();
        match result {
            Ok(report) => self.go(Page::Done { report, pack }),
            Err(ImportError::Cancelled) => {
                self.note = Some(Note::Cancelled);
                self.go(Page::Choose);
            }
            Err(e) => {
                let text = screens::error_text(self.lang, &e);
                self.go(Page::Failed { source, text });
            }
        }
    }

    /// Back to the start of setup for `reason` (the data dir was checked again).
    pub fn restart(&mut self, reason: SetupReason) {
        if reason == SetupReason::ImportRunning && matches!(self.page, Page::Waiting) {
            self.note = Some(Note::StillRunning);
            self.dirty = true;
        } else {
            self.note = None;
            let page = Self::first_page(&reason);
            self.go(page);
        }
        self.reason = reason;
    }

    /// The current page as a 640x480 display list.
    pub fn frame(&self, font: &Font) -> Frame {
        screens::frame(self, font)
    }
}

/// `setup::import` on a worker thread.
pub struct ImportJob {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<Progress>>>,
    thread: Option<JoinHandle<Result<ImportReport, ImportError>>>,
    data_dir: PathBuf,
}

/// How long quitting waits for a cancelled import to stop.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// Shortest interval between progress wakeups of the main loop.
const PROGRESS_WAKE: Duration = Duration::from_millis(100);

impl ImportJob {
    /// Starts importing `source` into `data_dir`; `wake` is called on progress (at most
    /// every [`PROGRESS_WAKE`]) and when the import ends.
    pub fn start(source: Source, data_dir: PathBuf, wake: Wake) -> std::io::Result<ImportJob> {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(None));
        let (c, p, dir) = (cancel.clone(), progress.clone(), data_dir.clone());
        let thread = std::thread::Builder::new()
            .name("import".into())
            .spawn(move || {
                let mut last: Option<Instant> = None;
                let mut report = |pr: &Progress| {
                    *lock(&p) = Some(*pr);
                    if last.is_none_or(|t| t.elapsed() >= PROGRESS_WAKE) {
                        last = Some(Instant::now());
                        wake();
                    }
                };
                let r = setup::import(&source, &dir, &setup::TOPCD, &mut report, &c);
                wake();
                r
            })?;
        Ok(ImportJob {
            cancel,
            progress,
            thread: Some(thread),
            data_dir,
        })
    }

    /// The latest progress report.
    pub fn progress(&self) -> Option<Progress> {
        *lock(&self.progress)
    }

    /// Asks the import to stop; it ends with `Cancelled` at the next chunk.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The result once the worker has finished.
    pub fn try_finish(&mut self) -> Option<Result<ImportReport, ImportError>> {
        if !self.thread.as_ref()?.is_finished() {
            return None;
        }
        Some(self.join())
    }

    /// Cancels and waits up to `limit` for the worker (quitting during an import).
    /// `None` when it did not stop in time: the worker is left to the process exit, and
    /// the next start's `setup::recover` cleans up what it leaves.
    pub fn cancel_and_wait(mut self, limit: Duration) -> Option<Result<ImportReport, ImportError>> {
        self.cancel();
        let deadline = Instant::now() + limit;
        while self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            if Instant::now() >= deadline {
                // Detach: dropping the handle does not wait.
                drop(self.thread.take());
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Some(self.join())
    }

    fn join(&mut self) -> Result<ImportReport, ImportError> {
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(_)) | None => Err(ImportError::Io {
                path: self.data_dir.clone(),
                source: std::io::Error::other("the import thread stopped unexpectedly"),
            }),
        }
    }
}

impl Drop for ImportJob {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.cancel();
            let _ = t.join();
        }
    }
}

/// Where the upscale worker finds the runner. With the `test-hooks` feature,
/// `MILIONAR_RUNNER_DIR` names the directory (to test the failure pages).
fn runner_source() -> RunnerSource {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = std::env::var_os("MILIONAR_RUNNER_DIR") {
        return RunnerSource::Dir(PathBuf::from(d));
    }
    RunnerSource::Auto
}

/// What the app does after a setup event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    None,
    Quit,
    /// Check the data dir and start the game (after an import, or a retry).
    StartGame,
    /// Open the options overlay over the wizard.
    OpenOptions,
}

/// What the main loop lends to setup for an event.
pub struct Ctx<'a> {
    pub window: &'a sdl3::video::Window,
    /// Makes a callback that wakes the main loop.
    pub waker: &'a dyn Fn() -> Wake,
}

enum Picked {
    Path(PathBuf),
    Canceled,
    Failed(String),
}

/// Setup mode of the app: the wizard plus its dialogs and import worker.
pub struct SetupMode {
    wizard: Wizard,
    data_dir: PathBuf,
    job: Option<ImportJob>,
    upscale: Option<UpscaleJob>,
    picked_tx: Sender<Picked>,
    picked_rx: Receiver<Picked>,
    dialog_open: bool,
    /// The page last drawn (page changes are logged).
    shown: Option<PageKind>,
}

impl SetupMode {
    pub fn new(lang: Lang, reason: SetupReason, data_dir: &Path) -> SetupMode {
        let (picked_tx, picked_rx) = channel();
        let mut wizard = Wizard::new(lang, reason.clone());
        if reason == SetupReason::Rebuild {
            wizard.set_offer_pack(crate::art::open_pack(data_dir).kind());
        }
        SetupMode {
            wizard,
            data_dir: data_dir.to_path_buf(),
            job: None,
            upscale: None,
            picked_tx,
            picked_rx,
            dialog_open: false,
            shown: None,
        }
    }

    #[cfg(feature = "test-hooks")]
    pub fn wizard(&self) -> &Wizard {
        &self.wizard
    }

    /// The data dir was checked again and setup continues for `reason`.
    pub fn restart(&mut self, reason: SetupReason) {
        self.wizard.restart(reason);
    }

    /// The options overlay over the wizard asked to import again.
    pub fn reimport(&mut self) {
        if self.job.is_none() && self.upscale.is_none() {
            self.wizard.reimport();
        }
    }

    /// The options overlay over the wizard asked to make the enhanced graphics again.
    pub fn rebuild(&mut self) {
        if self.job.is_none() && self.upscale.is_none() {
            let pack = crate::art::open_pack(&self.data_dir).kind();
            self.wizard.rebuild(pack);
        }
    }

    /// Forces a redraw of the page (after the overlay over it closed).
    pub fn touch(&mut self) {
        self.wizard.dirty = true;
    }

    pub fn key(&mut self, nav: Nav, ctx: &Ctx<'_>) -> Outcome {
        let e = self.wizard.key(nav);
        self.apply(e, ctx)
    }

    pub fn pointer(&mut self, x: i32, y: i32) {
        self.wizard.pointer(x, y);
    }

    pub fn click(&mut self, x: i32, y: i32, ctx: &Ctx<'_>) -> Outcome {
        let e = self.wizard.click(x, y);
        self.apply(e, ctx)
    }

    /// A file or folder dropped onto the window.
    pub fn dropped(&mut self, path: &Path, ctx: &Ctx<'_>) -> Outcome {
        crate::log::log(&format!("setup: dropped {}", path.display()));
        let e = self.wizard.choose(path);
        self.apply(e, ctx)
    }

    /// Dialog results and import progress; call every main-loop iteration.
    pub fn poll(&mut self, ctx: &Ctx<'_>) -> Outcome {
        let mut out = Outcome::None;
        while let Ok(p) = self.picked_rx.try_recv() {
            self.dialog_open = false;
            match p {
                Picked::Path(path) => {
                    crate::log::log(&format!("setup: chosen {}", path.display()));
                    let e = self.wizard.choose(&path);
                    out = self.apply(e, ctx);
                }
                Picked::Canceled => {}
                Picked::Failed(why) => {
                    crate::log::log(&format!("setup: file dialog failed: {why}"));
                    self.wizard.dialog_failed(why);
                }
            }
        }
        if let Some(job) = &mut self.job {
            if let Some(p) = job.progress() {
                self.wizard.progress(p);
            }
            if let Some(result) = job.try_finish() {
                self.job = None;
                self.import_ended(result);
            }
        }
        if let Some(job) = &mut self.upscale {
            self.wizard.upscale_status(job.status());
            if let Some(result) = job.try_finish() {
                self.upscale = None;
                match &result {
                    Ok(_) => {}
                    Err(e) => crate::log::log(&format!("upscale: ended: {e}")),
                }
                self.wizard.upscale_finished(result);
            }
        }
        out
    }

    fn import_ended(&mut self, result: Result<ImportReport, ImportError>) {
        let pack_ready = match &result {
            Ok(r) => {
                crate::log::log(&format!(
                    "setup: import done: {} files, {} bytes from {}",
                    r.files,
                    r.bytes,
                    r.source.path().display()
                ));
                let status = crate::art::open_pack(&self.data_dir);
                crate::log::log(&format!("setup: {}", status.describe(&self.data_dir)));
                status.kind()
            }
            Err(e) => {
                crate::log::log(&format!("setup: import ended: {e}"));
                PackKind::Missing
            }
        };
        self.wizard.finished(result, pack_ready);
    }

    /// The page as a display list when it changed since the last call.
    pub fn frame_if_dirty(&mut self, font: &Font) -> Option<Frame> {
        if !self.wizard.take_dirty() {
            return None;
        }
        let page = self.wizard.page();
        if self.shown != Some(page) {
            self.shown = Some(page);
            crate::log::log(&format!("setup: page {page:?}"));
        }
        Some(self.wizard.frame(font))
    }

    fn apply(&mut self, effect: Effect, ctx: &Ctx<'_>) -> Outcome {
        match effect {
            Effect::None => Outcome::None,
            Effect::Quit => Outcome::Quit,
            Effect::StartGame | Effect::RetryStartup => Outcome::StartGame,
            Effect::PickFile | Effect::PickFolder => {
                #[cfg(feature = "test-hooks")]
                if let Some(p) = std::env::var_os("MILIONAR_PICK") {
                    let _ = self.picked_tx.send(Picked::Path(PathBuf::from(p)));
                    return Outcome::None;
                }
                if !self.dialog_open {
                    self.open_dialog(effect == Effect::PickFolder, ctx);
                }
                Outcome::None
            }
            Effect::StartImport(source) => {
                if self.job.is_some() {
                    return Outcome::None;
                }
                crate::log::log(&format!(
                    "setup: importing from {} {}",
                    source.kind(),
                    source.path().display()
                ));
                match ImportJob::start(source, self.data_dir.clone(), (ctx.waker)()) {
                    Ok(job) => self.job = Some(job),
                    Err(e) => self.import_ended(Err(ImportError::Io {
                        path: self.data_dir.clone(),
                        source: e,
                    })),
                }
                Outcome::None
            }
            Effect::CancelImport => {
                if let Some(job) = &self.job {
                    crate::log::log("setup: cancelling the import");
                    job.cancel();
                }
                Outcome::None
            }
            Effect::OpenOptions => Outcome::OpenOptions,
            Effect::StartUpscale { allow_cpu } => {
                if self.upscale.is_some() || self.job.is_some() {
                    return Outcome::None;
                }
                crate::log::log(&format!(
                    "upscale: starting{}",
                    if allow_cpu {
                        " (CPU-only device accepted)"
                    } else {
                        ""
                    }
                ));
                match UpscaleJob::start(
                    self.data_dir.clone(),
                    runner_source(),
                    allow_cpu,
                    (ctx.waker)(),
                ) {
                    Ok(job) => self.upscale = Some(job),
                    Err(e) => self
                        .wizard
                        .upscale_finished(Err(UpscaleFail::Error(e.into()))),
                }
                Outcome::None
            }
            Effect::CancelUpscale => {
                if let Some(job) = &self.upscale {
                    crate::log::log("upscale: cancelling");
                    job.cancel();
                }
                Outcome::None
            }
        }
    }

    fn open_dialog(&mut self, folder: bool, ctx: &Ctx<'_>) {
        let tx = self.picked_tx.clone();
        let wake = (ctx.waker)();
        let callback: sdl3::dialog::DialogCallback = Box::new(move |result, _filter| {
            let picked = match result {
                Ok(paths) => paths
                    .into_iter()
                    .next()
                    .map_or(Picked::Canceled, Picked::Path),
                Err(sdl3::dialog::DialogError::Canceled) => Picked::Canceled,
                Err(e) => Picked::Failed(e.to_string()),
            };
            let _ = tx.send(picked);
            wake();
        });
        self.dialog_open = true;
        if folder {
            // A mounted disc is under /Volumes on macOS.
            let start = Path::new("/Volumes");
            let start = (cfg!(target_os = "macos") && start.is_dir()).then_some(start);
            sdl3::dialog::show_open_folder_dialog(start, false, ctx.window, callback);
        } else {
            let lang = self.wizard.lang();
            let filters = [
                sdl3::dialog::DialogFileFilter {
                    name: text(lang, Str::FilterSource),
                    pattern: "iso;exe",
                },
                sdl3::dialog::DialogFileFilter {
                    name: text(lang, Str::FilterAll),
                    pattern: "*",
                },
            ];
            if let Err(e) = sdl3::dialog::show_open_file_dialog(
                &filters,
                None::<&Path>,
                false,
                ctx.window,
                callback,
            ) {
                self.dialog_open = false;
                self.wizard.dialog_failed(e.to_string());
            }
        }
    }

    /// Leaves setup: a running import is cancelled and waited for (up to
    /// [`QUIT_WAIT`]), so quitting leaves no partial `game/` and no worker behind. An
    /// import stuck in I/O past that is abandoned; it never replaces `game/` with a
    /// partial tree, and `setup::recover` removes its staging at the next start.
    pub fn shutdown(mut self) {
        if let Some(job) = self.upscale.take() {
            crate::log::log("upscale: leaving during the upscale, cancelling it");
            match job.cancel_and_wait(QUIT_WAIT) {
                Some(Ok(_)) => crate::log::log("upscale: it had already finished"),
                Some(Err(e)) => crate::log::log(&format!("upscale: stopped: {e}")),
                None => crate::log::log(&format!(
                    "upscale: did not stop within {} s; exiting anyway (the runner dies with the app)",
                    QUIT_WAIT.as_secs()
                )),
            }
        }
        if let Some(job) = self.job.take() {
            crate::log::log("setup: quitting during the import, cancelling it");
            match job.cancel_and_wait(QUIT_WAIT) {
                Some(Ok(_)) => crate::log::log("setup: the import had already finished"),
                Some(Err(e)) => crate::log::log(&format!("setup: import stopped: {e}")),
                None => crate::log::log(&format!(
                    "setup: the import did not stop within {} s; exiting anyway (the next start cleans up)",
                    QUIT_WAIT.as_secs()
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::Lang;
    use std::path::PathBuf;

    fn installed_dir(root: &Path) -> PathBuf {
        let d = root.join("Hra milionář");
        std::fs::create_dir_all(d.join("Audiolib")).unwrap();
        std::fs::write(d.join("Milionar.exe"), b"not the real one").unwrap();
        d
    }

    fn report(source: Source) -> ImportReport {
        ImportReport {
            source,
            files: 300,
            bytes: 600 << 20,
        }
    }

    fn center(r: render::Rect) -> (i32, i32) {
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    #[test]
    fn wizard_flow_state_machine() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = Wizard::new(Lang::Cs, SetupReason::NoImport);
        assert_eq!(w.page(), PageKind::Welcome);
        assert_eq!(w.actions(), vec![Action::Continue, Action::Quit]);
        // Focus moves and wraps both ways.
        assert_eq!(w.focus(), 0);
        assert_eq!(w.key(Nav::Next), Effect::None);
        assert_eq!(w.focus(), 1);
        assert_eq!(w.key(Nav::Next), Effect::None);
        assert_eq!(w.focus(), 0);
        assert_eq!(w.key(Nav::Prev), Effect::None);
        assert_eq!(w.focus(), 1);
        assert_eq!(w.key(Nav::Activate), Effect::Quit);
        // Esc on the first screen quits.
        assert_eq!(w.key(Nav::Back), Effect::Quit);

        // Welcome -> choose a source; Esc goes back.
        w.key(Nav::Prev);
        assert_eq!(w.key(Nav::Activate), Effect::None);
        assert_eq!(w.page(), PageKind::Choose);
        assert_eq!(
            w.actions(),
            vec![Action::PickFile, Action::PickFolder, Action::Back]
        );
        assert_eq!(w.focus(), 0, "focus resets on a new page");
        assert_eq!(w.key(Nav::Activate), Effect::PickFile);
        w.key(Nav::Next);
        assert_eq!(w.key(Nav::Activate), Effect::PickFolder);
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.page(), PageKind::Welcome);
        w.key(Nav::Activate);

        // Something that is not a source: a message, still choosing.
        let readme = tmp.path().join("README.md");
        std::fs::write(&readme, b"# readme").unwrap();
        assert_eq!(w.choose(&readme), Effect::None);
        assert_eq!(w.page(), PageKind::Choose);
        assert!(matches!(w.note(), Some(Note::NotRecognized(p)) if p == &readme));
        w.dialog_failed("no portal".into());
        assert!(matches!(w.note(), Some(Note::DialogFailed(r)) if r == "no portal"));

        // A source: the import starts; Esc cancels once.
        let dir = installed_dir(tmp.path());
        let src = Source::Installed(dir.clone());
        assert_eq!(w.choose(&dir), Effect::StartImport(src.clone()));
        assert_eq!(w.page(), PageKind::Importing);
        assert_eq!(w.note(), None);
        // Nothing else is picked while importing.
        assert_eq!(w.choose(&dir), Effect::None);
        assert!(w.progress(setup::Progress {
            done_bytes: 1,
            total_bytes: 2,
            current: "Milionar.exe"
        }));
        assert_eq!(w.key(Nav::Back), Effect::CancelImport);
        assert!(w.cancelling());
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.key(Nav::Activate), Effect::None);
        w.finished(Err(ImportError::Cancelled), PackKind::Missing);
        assert_eq!(w.page(), PageKind::Choose);
        assert_eq!(w.note(), Some(&Note::Cancelled));

        // A game running elsewhere: retry the same source.
        assert_eq!(w.choose(&dir), Effect::StartImport(src.clone()));
        w.finished(
            Err(ImportError::GameRunning(tmp.path().into())),
            PackKind::Missing,
        );
        assert_eq!(w.page(), PageKind::Failed);
        assert_eq!(
            w.actions(),
            vec![Action::Retry, Action::OtherSource, Action::Quit]
        );
        assert_eq!(w.key(Nav::Activate), Effect::StartImport(src.clone()));
        assert_eq!(w.page(), PageKind::Importing);
        assert!(!w.cancelling());

        // A failure retrying cannot fix: choose another source (or Esc).
        w.finished(
            Err(ImportError::Missing("Milionar.exe".into())),
            PackKind::Missing,
        );
        assert_eq!(w.actions(), vec![Action::OtherSource, Action::Quit]);
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.page(), PageKind::Choose);
        assert_eq!(w.note(), None);
        // A dropped file on the failure screen starts again too.
        w.choose(&dir);
        w.finished(Err(ImportError::Missing("x".into())), PackKind::Missing);
        assert_eq!(w.choose(&dir), Effect::StartImport(src.clone()));

        // Success without a valid upscaled pack: the upscale offer, then the game.
        w.finished(Ok(report(src.clone())), PackKind::Missing);
        assert_eq!(w.page(), PageKind::Done);
        assert_eq!(w.actions(), vec![Action::Continue, Action::Options]);
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.choose(&dir), Effect::None);
        assert_eq!(w.key(Nav::Activate), Effect::None);
        assert_eq!(w.page(), PageKind::Upscale);
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.page(), PageKind::Done);
        w.key(Nav::Activate);
        assert_eq!(w.actions(), vec![Action::RunUpscale, Action::SkipUpscale]);
        w.key(Nav::Next);
        assert_eq!(w.key(Nav::Activate), Effect::StartGame);

        // With a valid pack the game starts from the done screen.
        let mut w = Wizard::new(Lang::En, SetupReason::NoImport);
        w.key(Nav::Activate);
        w.choose(&dir);
        w.finished(Ok(report(src.clone())), PackKind::Ready);
        assert_eq!(w.actions(), vec![Action::StartGame, Action::Options]);
        assert_eq!(w.key(Nav::Activate), Effect::StartGame);
        // The game did not start after all (data unreadable): setup again, with the reason.
        w.restart(SetupReason::Unreadable("bad".into()));
        assert_eq!(w.page(), PageKind::Welcome);
        assert_eq!(w.reason(), &SetupReason::Unreadable("bad".into()));
    }

    #[test]
    fn wizard_waits_for_an_import_elsewhere() {
        let mut w = Wizard::new(Lang::Cs, SetupReason::ImportRunning);
        assert_eq!(w.page(), PageKind::Waiting);
        assert_eq!(w.actions(), vec![Action::RetryStartup, Action::Quit]);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(w.choose(&installed_dir(tmp.path())), Effect::None);
        assert_eq!(w.key(Nav::Activate), Effect::RetryStartup);
        w.restart(SetupReason::ImportRunning);
        assert_eq!(w.page(), PageKind::Waiting);
        assert_eq!(w.note(), Some(&Note::StillRunning));
        assert_eq!(w.key(Nav::Back), Effect::Quit);
        // The other import finished without leaving usable data: the normal start.
        w.restart(SetupReason::NoImport);
        assert_eq!(w.page(), PageKind::Welcome);
        assert_eq!(w.note(), None);
    }

    #[test]
    fn wizard_mouse_hover_and_click() {
        let mut w = Wizard::new(Lang::Cs, SetupReason::NoImport);
        let rects = widgets::button_rects(w.actions().len());
        let (x, y) = center(rects[1]);
        assert!(w.pointer(x, y));
        assert_eq!(w.focus(), 1);
        assert!(!w.pointer(x, y), "no change, no redraw");
        assert!(!w.pointer(5, 5), "outside keeps the focus");
        assert_eq!(w.focus(), 1);
        assert_eq!(w.click(5, 5), Effect::None);
        let (x, y) = center(rects[0]);
        assert_eq!(w.click(x, y), Effect::None);
        assert_eq!(w.page(), PageKind::Choose);
        let rects = widgets::button_rects(w.actions().len());
        let (x, y) = center(rects[1]);
        assert_eq!(w.click(x, y), Effect::PickFolder);
        assert!(w.take_dirty());
        assert!(!w.take_dirty());
    }

    #[test]
    fn import_job_cancel_joins_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data ěšč");
        let src = Source::Installed(installed_dir(tmp.path()));
        // Runs to its (failed) end and reports it.
        let mut job = ImportJob::start(src.clone(), data.clone(), Box::new(|| {})).unwrap();
        let t0 = std::time::Instant::now();
        let result = loop {
            if let Some(r) = job.try_finish() {
                break r;
            }
            assert!(t0.elapsed().as_secs() < 20);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(result.is_err());
        // Cancelled and joined (as on quit): nothing staged is left behind.
        let job = ImportJob::start(src, data.clone(), Box::new(|| {})).unwrap();
        let r = job.cancel_and_wait(Duration::from_secs(20));
        assert!(matches!(r, Some(Err(_))), "{r:?}");
        let left: Vec<_> = std::fs::read_dir(&data)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".staging") || n == "game")
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    fn at_offer(pack: PackKind) -> Wizard {
        let tmp = tempfile::tempdir().unwrap();
        let dir = installed_dir(tmp.path());
        let mut w = Wizard::new(Lang::Cs, SetupReason::NoImport);
        w.key(Nav::Activate);
        w.choose(&dir);
        w.finished(Ok(report(Source::Installed(dir))), pack);
        w.key(Nav::Activate);
        assert_eq!(w.page(), PageKind::Upscale);
        w
    }

    fn up_report() -> upscale_crate::Report {
        upscale_crate::Report {
            files: 88,
            frames: 3235,
            unique: 2450,
            upscaled: 2450,
            pack_bytes: 1 << 30,
            ..Default::default()
        }
    }

    #[test]
    fn upscale_flow_state_machine() {
        use upscale_crate::UpscaleError;
        let mut w = at_offer(PackKind::Stale);
        // Run: the worker starts; Esc cancels once; the offer comes back with a note.
        assert_eq!(
            w.key(Nav::Activate),
            Effect::StartUpscale { allow_cpu: false }
        );
        assert_eq!(w.page(), PageKind::Upscaling);
        assert_eq!(w.actions(), vec![Action::CancelUpscale]);
        assert!(w.upscale_status(UpscaleStatus::default()));
        assert!(
            !w.upscale_status(UpscaleStatus::default()),
            "no change, no redraw"
        );
        assert_eq!(w.key(Nav::Back), Effect::CancelUpscale);
        assert_eq!(w.key(Nav::Back), Effect::None);
        w.upscale_finished(Err(UpscaleFail::Error(UpscaleError::Cancelled)));
        assert_eq!(w.page(), PageKind::Upscale);
        assert!(
            matches!(w.page, Page::Upscale { cancelled: true, ref ctx } if ctx.pack == PackKind::Stale)
        );
        // A CPU-only device: warn, then run anyway or skip.
        w.key(Nav::Activate);
        w.upscale_finished(Err(UpscaleFail::CpuOnly("llvmpipe".into())));
        assert_eq!(w.page(), PageKind::UpscaleCpu);
        assert_eq!(w.actions(), vec![Action::RunAnyway, Action::SkipUpscale]);
        assert_eq!(
            w.key(Nav::Activate),
            Effect::StartUpscale { allow_cpu: true }
        );
        // No Vulkan: a clear failure with retry, or play with the original art.
        w.upscale_finished(Err(UpscaleFail::Error(UpscaleError::NoVulkan {
            stderr: "vkCreateInstance failed".into(),
        })));
        assert_eq!(w.page(), PageKind::UpscaleFailed);
        assert_eq!(w.actions(), vec![Action::Retry, Action::SkipUpscale]);
        assert_eq!(
            w.key(Nav::Activate),
            Effect::StartUpscale { allow_cpu: false }
        );
        // A missing tool cannot be retried; Esc goes back to the offer, then the import
        // result.
        w.upscale_finished(Err(UpscaleFail::NoRunner(vec![])));
        assert_eq!(w.actions(), vec![Action::SkipUpscale]);
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.page(), PageKind::Upscale);
        assert_eq!(w.key(Nav::Back), Effect::None);
        assert_eq!(w.page(), PageKind::Done);
        assert!(matches!(
            w.page,
            Page::Done {
                pack: PackKind::Stale,
                ..
            }
        ));
        // Success: the game starts with the pack; the options are one click away.
        w.key(Nav::Activate);
        w.key(Nav::Activate);
        // Results for a page that is not running are ignored.
        let mut idle = at_offer(PackKind::Missing);
        idle.upscale_finished(Ok(up_report()));
        assert_eq!(idle.page(), PageKind::Upscale);
        w.upscale_finished(Ok(up_report()));
        assert_eq!(w.page(), PageKind::UpscaleDone);
        assert_eq!(w.actions(), vec![Action::StartGame, Action::Options]);
        assert_eq!(w.key(Nav::Back), Effect::None);
        w.key(Nav::Next);
        assert_eq!(w.key(Nav::Activate), Effect::OpenOptions);
        w.key(Nav::Prev);
        assert_eq!(w.key(Nav::Activate), Effect::StartGame);

        // A rebuild from the game's options: Esc and the second button lead back to it.
        let mut w = Wizard::new(Lang::En, SetupReason::Rebuild);
        assert_eq!(w.page(), PageKind::Upscale);
        assert_eq!(w.actions(), vec![Action::RunUpscale, Action::BackToGame]);
        assert_eq!(w.key(Nav::Back), Effect::RetryStartup);
        w.key(Nav::Next);
        assert_eq!(w.key(Nav::Activate), Effect::RetryStartup);
        // Its failures and the CPU warning also lead back to the game, not "play with
        // the original graphics" (the old pack is still there).
        w.set_offer_pack(PackKind::Ready);
        w.key(Nav::Prev);
        w.key(Nav::Activate);
        w.upscale_finished(Err(UpscaleFail::Error(UpscaleError::RunnerFailed {
            code: 1,
            stderr: String::new(),
        })));
        assert_eq!(w.actions(), vec![Action::Retry, Action::BackToGame]);
        w.key(Nav::Activate);
        w.upscale_finished(Err(UpscaleFail::NoRunner(vec![])));
        assert_eq!(w.actions(), vec![Action::BackToGame]);
        assert_eq!(w.key(Nav::Activate), Effect::RetryStartup);
        w.key(Nav::Back);
        w.key(Nav::Activate);
        w.upscale_finished(Err(UpscaleFail::CpuOnly("llvmpipe".into())));
        assert_eq!(w.actions(), vec![Action::RunAnyway, Action::BackToGame]);
        // A re-import from the game: the source page, Esc leads back to the game.
        let mut w = Wizard::new(Lang::Cs, SetupReason::Reimport);
        assert_eq!(w.page(), PageKind::Choose);
        assert_eq!(
            w.actions(),
            vec![Action::PickFile, Action::PickFolder, Action::BackToGame]
        );
        assert_eq!(w.key(Nav::Back), Effect::RetryStartup);
        // The options over the wizard's last page: import again, or rebuild. No game has
        // been left, so nothing says "back to the game": the way out starts the game.
        let mut w = at_offer(PackKind::Missing);
        w.reimport();
        assert_eq!(w.page(), PageKind::Choose);
        assert_eq!(
            w.actions(),
            vec![Action::PickFile, Action::PickFolder, Action::StartGame]
        );
        assert_eq!(w.key(Nav::Back), Effect::RetryStartup);
        w.rebuild(PackKind::Ready);
        assert_eq!(w.page(), PageKind::Upscale);
        assert_eq!(w.actions(), vec![Action::RunUpscale, Action::StartGame]);
        assert_eq!(w.key(Nav::Back), Effect::RetryStartup);
        w.key(Nav::Activate);
        w.upscale_finished(Err(UpscaleFail::NoRunner(vec![])));
        assert_eq!(w.actions(), vec![Action::StartGame]);
        w.key(Nav::Back);
        w.key(Nav::Activate);
        w.upscale_finished(Err(UpscaleFail::CpuOnly("llvmpipe".into())));
        assert_eq!(w.actions(), vec![Action::RunAnyway, Action::StartGame]);
        // Without a pack the way out plays with the original graphics.
        w.rebuild(PackKind::Missing);
        assert_eq!(w.actions(), vec![Action::RunUpscale, Action::SkipUpscale]);
        for w in [&w, &{
            let mut r = at_offer(PackKind::Missing);
            r.reimport();
            r
        }] {
            let labels: Vec<&str> = w.actions().iter().map(|a| w.label(*a)).collect();
            assert!(!labels.contains(&w.label(Action::BackToGame)), "{labels:?}");
        }
    }

    /// Skipping the offer starts the game without running anything: no pack appears,
    /// and the game draws the original art even with the upscaled setting.
    #[test]
    fn upscale_offer_skipped_keeps_original() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("game")).unwrap();
        std::fs::write(data.join("game/import.json"), b"{}").unwrap();
        for pack in [PackKind::Missing, PackKind::Stale] {
            let mut w = at_offer(pack);
            let effects = vec![w.key(Nav::Next), w.key(Nav::Activate)];
            assert_eq!(effects, vec![Effect::None, Effect::StartGame]);
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::StartUpscale { .. })),
                "{effects:?}"
            );
        }
        let status = crate::art::open_pack(&data);
        assert_eq!(status.kind(), PackKind::Missing);
        assert!(!data.join("upscaled").exists());
        let setting = crate::settings::Settings::default().art_mode;
        assert_eq!(setting, crate::settings::ArtMode::Upscaled);
        assert_eq!(
            crate::art::effective(setting, status.into_pack().is_some()),
            crate::settings::ArtMode::Original
        );
    }
}
