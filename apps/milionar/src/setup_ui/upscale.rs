//! The upscale step of the wizard: the worker that makes the enhanced graphics, the
//! texts of its failures and the drawing of its progress.
//!
//! [`UpscaleJob`] runs on an "upscale" thread: runner lookup (next to the app, see
//! `upscale::runner::runner_dir_candidates`) and hash check, free space check (3.5 GB), a
//! GPU probe (a CPU-only Vulkan device stops here so the player can choose), then
//! `upscale::pipeline::run`. It never holds the import lock, and the game is never
//! running while it works (the app shuts the game down first), so no pack file is open
//! while the new one is renamed into place.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use upscale::pipeline::{Options, Phase, Progress, Report};
use upscale::{Runner, UpscaleError, Upscaler};

use super::screens::{ErrorText, human_size};
use super::widgets::{DIM, Page as Builder, TEXT, WARN};
use crate::host::{Wake, lock};
use crate::strings::{Lang, Str, fill, text};

/// Why the upscale step did not produce a pack.
#[derive(Debug)]
pub enum UpscaleFail {
    /// The Vulkan device is a software rasteriser (its name); the player decides.
    CpuOnly(String),
    /// No runner in any of these places.
    NoRunner(Vec<PathBuf>),
    /// A game (or an import) is running in another window of the app.
    GameRunning,
    Error(UpscaleError),
}

impl std::fmt::Display for UpscaleFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpscaleFail::CpuOnly(d) => write!(f, "the Vulkan device is CPU-only ({d})"),
            UpscaleFail::NoRunner(c) => {
                let list: Vec<String> = c.iter().map(|p| p.display().to_string()).collect();
                write!(f, "no upscaler runner; looked in {}", list.join(", "))
            }
            UpscaleFail::GameRunning => {
                write!(f, "a game or an import is running in another window")
            }
            UpscaleFail::Error(e) => e.fmt(f),
        }
    }
}

/// What the worker reports while it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The probed device, once known.
    pub device: Option<String>,
    /// The latest progress of each phase, in [`Phase`] order.
    pub phases: [Option<Progress>; 3],
}

impl Status {
    pub fn current(&self) -> Option<&Progress> {
        self.phases.iter().rev().flatten().next()
    }

    fn update(&mut self, p: &Progress) {
        let i = phase_index(p.phase);
        // Back in an earlier phase (a damaged cache record): later phases start over.
        for later in &mut self.phases[i + 1..] {
            *later = None;
        }
        self.phases[i] = Some(p.clone());
    }
}

fn phase_index(p: Phase) -> usize {
    match p {
        Phase::Scan => 0,
        Phase::Upscale => 1,
        Phase::Assemble => 2,
    }
}

/// Where the worker looks for the runner.
#[derive(Debug, Clone)]
pub enum RunnerSource {
    /// Next to the running executable (the app layouts and the dev tree).
    Auto,
    /// This directory (testing).
    #[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
    Dir(PathBuf),
}

/// The upscale worker.
pub struct UpscaleJob {
    cancel: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<Result<Report, UpscaleFail>>>,
}

/// Shortest interval between progress wakeups of the main loop.
const PROGRESS_WAKE: Duration = Duration::from_millis(100);

impl UpscaleJob {
    /// Starts making the pack for the import in `data_dir`. Without `allow_cpu` a
    /// CPU-only device ends the job with [`UpscaleFail::CpuOnly`] before any work.
    pub fn start(
        data_dir: PathBuf,
        runner: RunnerSource,
        allow_cpu: bool,
        wake: Wake,
    ) -> std::io::Result<UpscaleJob> {
        let cancel = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        let (c, st) = (cancel.clone(), status.clone());
        let thread = std::thread::Builder::new()
            .name("upscale".into())
            .spawn(move || {
                let r = work(&data_dir, &runner, allow_cpu, &st, &c, &wake);
                wake();
                r
            })?;
        Ok(UpscaleJob {
            cancel,
            status,
            thread: Some(thread),
        })
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The result once the worker has finished.
    pub fn try_finish(&mut self) -> Option<Result<Report, UpscaleFail>> {
        if !self.thread.as_ref()?.is_finished() {
            return None;
        }
        Some(self.join())
    }

    /// Cancels and waits up to `limit` (quitting). Cancelling kills the runner within
    /// about 50 ms, so this normally returns at once; `None` when it did not stop.
    pub fn cancel_and_wait(mut self, limit: Duration) -> Option<Result<Report, UpscaleFail>> {
        self.cancel();
        let deadline = Instant::now() + limit;
        while self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            if Instant::now() >= deadline {
                drop(self.thread.take());
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Some(self.join())
    }

    fn join(&mut self) -> Result<Report, UpscaleFail> {
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(_)) | None => Err(UpscaleFail::Error(UpscaleError::Game(
                "the upscale thread stopped unexpectedly".into(),
            ))),
        }
    }
}

impl Drop for UpscaleJob {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.cancel();
            let _ = t.join();
        }
    }
}

/// The directory of this executable, symlinks resolved, without the verbatim
/// `\\?\` prefix that canonicalize gives on Windows (the runner cannot take it).
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .map(|e| {
            e.canonicalize()
                .map(|c| upscale::runner::plain_path(&c))
                .unwrap_or(e)
        })
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The playing lock, exclusively: `GameRunning` while a game (or an import) runs in
/// another window.
fn lock_playing(data_dir: &Path) -> Result<setup::PlayingLock, UpscaleFail> {
    match setup::PlayingLock::acquire_exclusive(data_dir) {
        Ok(l) => Ok(l),
        Err(setup::ImportError::GameRunning(_) | setup::ImportError::Busy(_)) => {
            Err(UpscaleFail::GameRunning)
        }
        Err(setup::ImportError::Io { source, .. }) => Err(UpscaleFail::Error(source.into())),
        Err(e) => Err(UpscaleFail::Error(UpscaleError::Io(std::io::Error::other(
            e.to_string(),
        )))),
    }
}

fn work(
    data_dir: &Path,
    runner: &RunnerSource,
    allow_cpu: bool,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
    wake: &Wake,
) -> Result<Report, UpscaleFail> {
    let fail = UpscaleFail::Error;
    // Held for the whole job: no game reads `game/` or holds the pack open (another
    // window could, and Windows cannot replace an open pack), and no import swaps
    // `game/` between the pipeline's two passes.
    let _playing = lock_playing(data_dir)?;
    let dir = match runner {
        RunnerSource::Auto => {
            upscale::runner::find_runner_dir(&exe_dir()).map_err(UpscaleFail::NoRunner)?
        }
        RunnerSource::Dir(d) => d.clone(),
    };
    crate::log::log(&format!("upscale: runner {}", dir.display()));
    let runner = Runner::locate(&dir).map_err(fail)?;
    let needed = upscale::pipeline::free_space_needed(
        data_dir,
        &runner.params(),
        upscale::runner::FREE_SPACE_NEEDED,
    );
    if needed > 0 {
        upscale::runner::check_space(data_dir, needed).map_err(fail)?;
    }
    run_with(data_dir, &runner, allow_cpu, status, cancel, wake)
}

/// The probe and the pipeline with any upscaler (the tests use the fake one).
pub fn run_with(
    data_dir: &Path,
    up: &dyn Upscaler,
    allow_cpu: bool,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
    wake: &Wake,
) -> Result<Report, UpscaleFail> {
    let fail = UpscaleFail::Error;
    let probe = up.probe(cancel).map_err(fail)?;
    crate::log::log(&format!(
        "upscale: device {}{}",
        probe.device.as_deref().unwrap_or("(not reported)"),
        if probe.cpu_only { " (CPU only)" } else { "" }
    ));
    lock(status).device = probe.device.clone();
    wake();
    if probe.cpu_only && !allow_cpu {
        return Err(UpscaleFail::CpuOnly(probe.device.unwrap_or_default()));
    }
    let game =
        gamedata::GameData::open(data_dir).map_err(|e| fail(UpscaleError::Game(e.to_string())))?;
    let mut last: Option<Instant> = None;
    let mut progress = |p: &Progress| {
        lock(status).update(p);
        let end = p.done == p.total;
        if end || last.is_none_or(|t| t.elapsed() >= PROGRESS_WAKE) {
            last = Some(Instant::now());
            wake();
        }
    };
    let r = upscale::pipeline::run(
        &game,
        data_dir,
        up,
        &Options::default(),
        &mut progress,
        cancel,
    )
    .map_err(fail)?;
    crate::log::log(&format!(
        "upscale: done: {} frames of {} files ({} unique: {} upscaled, {} from cache, {} from the old pack), {} bytes in {} s{}",
        r.frames - r.conflicts,
        r.files,
        r.unique,
        r.upscaled,
        r.reused,
        r.seeded,
        r.pack_bytes,
        r.elapsed.as_secs(),
        if r.up_to_date {
            " (was up to date)"
        } else {
            ""
        }
    ));
    Ok(r)
}

/// The player's explanation of a failure (every variant).
pub fn fail_text(lang: Lang, f: &UpscaleFail) -> ErrorText {
    let t = |s| text(lang, s).to_string();
    let tool = |details: String| ErrorText {
        lines: vec![t(Str::UpErrTool), t(Str::UpErrToolHint)],
        details: Some(details),
        retry: false,
    };
    let e = match f {
        UpscaleFail::CpuOnly(d) => {
            return ErrorText {
                lines: vec![
                    fill(text(lang, Str::CpuBody), &[("device", d)]),
                    t(Str::CpuChoice),
                ],
                details: None,
                retry: true,
            };
        }
        UpscaleFail::NoRunner(_) => return tool(f.to_string()),
        UpscaleFail::GameRunning => {
            return ErrorText {
                lines: vec![t(Str::UpErrGameRunning)],
                details: None,
                retry: true,
            };
        }
        UpscaleFail::Error(e) => e,
    };
    let (lines, details, retry) = match e {
        UpscaleError::NoVulkan { stderr } => (
            vec![t(Str::UpErrNoVulkan), t(Str::UpErrNoVulkanHint)],
            Some(stderr.clone()).filter(|s| !s.is_empty()),
            true,
        ),
        UpscaleError::UnsupportedPlatform
        | UpscaleError::RunnerMissing(_)
        | UpscaleError::HashMismatch { .. }
        | UpscaleError::Spawn { .. } => return tool(e.to_string()),
        UpscaleError::RunnerCrashed { .. }
        | UpscaleError::RunnerFailed { .. }
        | UpscaleError::MissingOutput { .. }
        | UpscaleError::BadOutput { .. } => (
            vec![t(Str::UpErrCrashed), t(Str::UpErrCrashedHint)],
            Some(e.to_string()),
            true,
        ),
        UpscaleError::NoSpace { needed, available } => (
            vec![
                fill(
                    text(lang, Str::ErrNoSpace),
                    &[
                        ("needed", &human_size(lang, *needed)),
                        ("available", &human_size(lang, *available)),
                    ],
                ),
                t(Str::ErrNoSpaceHint),
            ],
            None,
            true,
        ),
        UpscaleError::Busy(_) => (vec![t(Str::UpErrBusy)], None, true),
        UpscaleError::Io(_) | UpscaleError::Pack(_) => {
            (vec![t(Str::UpErrFiles)], Some(e.to_string()), true)
        }
        UpscaleError::Game(_) => (
            vec![t(Str::UpErrGame), t(Str::UpErrGameHint)],
            Some(e.to_string()),
            false,
        ),
        UpscaleError::Cancelled => (vec![t(Str::UpscaleCancelled)], None, true),
    };
    ErrorText {
        lines,
        details,
        retry,
    }
}

/// A duration as "8 min", "45 s" or "2 h 10 min" (rounded up to whole minutes from
/// one minute on).
pub fn human_time(lang: Lang, d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        return fill(text(lang, Str::Seconds), &[("n", &s.max(1).to_string())]);
    }
    let m = s.div_ceil(60);
    if m < 60 {
        return fill(text(lang, Str::Minutes), &[("n", &m.to_string())]);
    }
    fill(
        text(lang, Str::Hours),
        &[("h", &(m / 60).to_string()), ("m", &(m % 60).to_string())],
    )
}

/// The overall estimate: the current phase's, plus a rough share for the saving phase
/// while upscaling (it takes about a tenth of the upscaling).
pub fn overall_eta(status: &Status) -> Option<Duration> {
    let p = status.current()?;
    let eta = p.eta?;
    Some(match p.phase {
        Phase::Scan => return None,
        Phase::Upscale => eta + p.elapsed.min(eta) / 10,
        Phase::Assemble => eta,
    })
}

/// The running page's body: device, one bar per phase, the estimate.
pub fn draw_running(p: &mut Builder<'_>, lang: Lang, status: Option<&Status>, cancelling: bool) {
    let t = |s| text(lang, s);
    let Some(st) = status.filter(|s| s.device.is_some() || s.current().is_some()) else {
        p.para(t(Str::RunChecking), TEXT);
        p.small(t(Str::RunKeeps), DIM);
        return;
    };
    if let Some(d) = &st.device {
        p.small(&fill(t(Str::RunDevice), &[("device", d)]), DIM);
    }
    for (i, label) in [Str::PhaseScan, Str::PhaseUpscale, Str::PhaseAssemble]
        .into_iter()
        .enumerate()
    {
        let pr = st.phases[i].as_ref();
        let active = st.current().is_some_and(|c| phase_index(c.phase) == i);
        let amount = pr.map(|pr| {
            fill(
                t(Str::RunAmount),
                &[
                    ("done", &pr.done.to_string()),
                    ("total", &pr.total.to_string()),
                ],
            )
        });
        let line = match amount {
            Some(a) => format!("{}   {a}", t(label)),
            None => t(label).to_string(),
        };
        p.small(&line, if active { TEXT } else { DIM });
        p.thin_bar(pr.map_or(0.0, |pr| pr.done as f64 / pr.total.max(1) as f64));
    }
    if cancelling {
        p.para(t(Str::RunCancelling), WARN);
        return;
    }
    match overall_eta(st) {
        Some(d) => p.para(
            &fill(t(Str::RunEta), &[("time", &human_time(lang, d))]),
            TEXT,
        ),
        None => p.para(t(Str::RunEtaUnknown), TEXT),
    }
    p.small(t(Str::RunKeeps), DIM);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_human() {
        let s = |n| human_time(Lang::En, Duration::from_secs(n));
        assert_eq!(s(0), "1 s");
        assert_eq!(s(45), "45 s");
        assert_eq!(s(61), "2 min");
        assert_eq!(s(480), "8 min");
        assert_eq!(s(3 * 3600 + 600), "3 h 10 min");
        assert_eq!(human_time(Lang::Cs, Duration::from_secs(480)), "8\u{a0}min");
    }

    #[test]
    fn status_keeps_each_phase() {
        let p = |phase, done, total| Progress {
            phase,
            done,
            total,
            elapsed: Duration::from_secs(done as u64),
            eta: Some(Duration::from_secs(10)),
        };
        let mut st = Status::default();
        assert!(st.current().is_none() && overall_eta(&st).is_none());
        st.update(&p(Phase::Scan, 3, 3));
        assert_eq!(overall_eta(&st), None, "no estimate while scanning");
        st.update(&p(Phase::Upscale, 5, 22));
        assert_eq!(st.current().unwrap().phase, Phase::Upscale);
        assert_eq!(overall_eta(&st), Some(Duration::from_millis(10_500)));
        st.update(&p(Phase::Assemble, 2, 30));
        assert_eq!(overall_eta(&st), Some(Duration::from_secs(10)));
        // A damaged record sends the run back to upscaling: saving starts over.
        st.update(&p(Phase::Upscale, 21, 22));
        assert!(st.phases[2].is_none());
        assert_eq!(st.phases[0].as_ref().unwrap().done, 3);
    }

    fn all_fails() -> Vec<UpscaleFail> {
        let p = PathBuf::from(
            "/Users/někdo/Library/Application Support/milionar-port/Milionar/upscaled",
        );
        let e = UpscaleFail::Error;
        vec![
            UpscaleFail::CpuOnly("llvmpipe (LLVM 15.0.7, 256 bits)".into()),
            UpscaleFail::NoRunner(vec![p.clone(), p.join("x")]),
            UpscaleFail::GameRunning,
            e(UpscaleError::Io(std::io::Error::other("disk on fire"))),
            e(UpscaleError::Pack(upscale::PackError::BadMagic)),
            e(UpscaleError::Game("Cuts/Logos.smk: bad header".into())),
            e(UpscaleError::UnsupportedPlatform),
            e(UpscaleError::RunnerMissing(p.clone())),
            e(UpscaleError::HashMismatch {
                path: p.clone(),
                expected: "a".repeat(64),
                found: "b".repeat(64),
            }),
            e(UpscaleError::Spawn {
                path: p.clone(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            }),
            e(UpscaleError::NoVulkan {
                stderr: "vkCreateInstance failed -9".into(),
            }),
            e(UpscaleError::RunnerCrashed {
                how: "signal 11".into(),
                stderr: "".into(),
            }),
            e(UpscaleError::RunnerFailed {
                code: 255,
                stderr: "boom".into(),
            }),
            e(UpscaleError::MissingOutput {
                file: "ab.png".into(),
                stderr: "".into(),
            }),
            e(UpscaleError::BadOutput {
                file: "ab.png".into(),
                reason: "size".into(),
            }),
            e(UpscaleError::Cancelled),
            e(UpscaleError::Busy(p)),
            e(UpscaleError::NoSpace {
                needed: 3584 << 20,
                available: 900 << 20,
            }),
        ]
    }

    #[test]
    fn fail_texts_cover_all_upscale_errors() {
        for lang in [Lang::Cs, Lang::En] {
            for f in all_fails() {
                let t = fail_text(lang, &f);
                let all = t.lines.join("\n");
                assert!(
                    !t.lines.is_empty() && t.lines.iter().all(|l| !l.is_empty()),
                    "{f:?}"
                );
                assert!(!all.contains('{'), "unfilled placeholder: {all}");
                // Technical text stays in the details line.
                for tech in [
                    "disk on fire",
                    "bad header",
                    "vkCreateInstance",
                    "boom",
                    "signal 11",
                ] {
                    assert!(!all.contains(tech), "{all}");
                }
                match &f {
                    UpscaleFail::CpuOnly(d) => assert!(all.contains(d.as_str())),
                    UpscaleFail::Error(UpscaleError::NoSpace { .. }) => {
                        assert!(all.contains("3,5 GB") || all.contains("3.5 GB"), "{all}");
                        assert!(all.contains("900 MB"), "{all}");
                    }
                    UpscaleFail::Error(UpscaleError::NoVulkan { .. }) => {
                        assert!(all.contains("Vulkan"));
                        assert!(t.details.as_deref().unwrap().contains("vkCreateInstance"));
                    }
                    _ => {}
                }
                let no_retry = matches!(
                    f,
                    UpscaleFail::NoRunner(_)
                        | UpscaleFail::Error(
                            UpscaleError::UnsupportedPlatform
                                | UpscaleError::RunnerMissing(_)
                                | UpscaleError::HashMismatch { .. }
                                | UpscaleError::Spawn { .. }
                                | UpscaleError::Game(_)
                        )
                );
                assert_eq!(t.retry, !no_retry, "{f:?}");
            }
        }
    }

    /// The worker with the test stand-in: the pack is written (rejected by the game as
    /// not pinned), progress and the device are reported, and a CPU-only device stops
    /// before any work unless allowed.
    #[test]
    fn job_runs_the_pipeline_and_stops_for_a_cpu_device() {
        struct Cpu(upscale::FakeRunner);
        impl Upscaler for Cpu {
            fn params(&self) -> upscale::RunnerParams {
                upscale::FakeRunner::fake_params()
            }
            fn upscale_dir(
                &self,
                i: &Path,
                o: &Path,
                c: &AtomicBool,
            ) -> upscale::runner::Result<()> {
                self.0.upscale_dir(i, o, c)
            }
            fn probe(&self, _: &AtomicBool) -> upscale::runner::Result<upscale::GpuProbe> {
                Ok(upscale::GpuProbe {
                    device: Some("llvmpipe".into()),
                    cpu_only: true,
                })
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data ěšč");
        std::fs::create_dir_all(d.join("game")).unwrap();
        std::fs::write(d.join("game/import.json"), b"{}").unwrap();
        let st = Mutex::new(Status::default());
        let cancel = AtomicBool::new(false);
        let wake: Wake = Box::new(|| {});
        let cpu = Cpu(upscale::FakeRunner::new());
        let r = run_with(&d, &cpu, false, &st, &cancel, &wake);
        assert!(
            matches!(r, Err(UpscaleFail::CpuOnly(ref n)) if n == "llvmpipe"),
            "{r:?}"
        );
        assert_eq!(cpu.0.invocations.load(Ordering::Relaxed), 0);
        assert!(!upscale::pack_path(&d).exists());
        // Allowed: runs (an empty game has no frames, the pack is still written).
        let r = run_with(&d, &cpu, true, &st, &cancel, &wake).unwrap();
        assert_eq!(r.frames, 0);
        assert!(upscale::pack_path(&d).is_file());
        let st = st.into_inner().unwrap();
        assert_eq!(st.device.as_deref(), Some("llvmpipe"));
        assert!(st.phases.iter().all(Option::is_some), "{st:?}");
        // Cancelled before it starts.
        cancel.store(true, Ordering::Relaxed);
        std::fs::remove_file(upscale::pack_path(&d)).unwrap();
        let fake = upscale::FakeRunner::new();
        let st = Mutex::new(Status::default());
        let r = run_with(&d, &fake, false, &st, &cancel, &wake);
        // An empty game has nothing to cancel in; either way no runner ran.
        assert!(
            matches!(r, Err(UpscaleFail::Error(UpscaleError::Cancelled)) | Ok(_)),
            "{r:?}"
        );
        assert_eq!(fake.invocations.load(Ordering::Relaxed), 0);
    }

    /// A game playing in another window: the job stops before any work (no runner
    /// lookup, no `upscaled/`), with a message that can be retried once it is closed.
    #[test]
    fn job_refused_while_a_game_plays() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data");
        std::fs::create_dir_all(d.join("game")).unwrap();
        std::fs::write(d.join("game/import.json"), b"{}").unwrap();
        let finish = |job: &mut UpscaleJob| {
            let t0 = Instant::now();
            loop {
                if let Some(r) = job.try_finish() {
                    break r;
                }
                assert!(t0.elapsed().as_secs() < 20);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let game = setup::PlayingLock::acquire(&d).unwrap();
        let start = || {
            UpscaleJob::start(
                d.clone(),
                RunnerSource::Dir(tmp.path().join("no runner here")),
                false,
                Box::new(|| {}),
            )
            .unwrap()
        };
        let r = finish(&mut start()).unwrap_err();
        assert!(matches!(r, UpscaleFail::GameRunning), "{r:?}");
        let text = fail_text(Lang::Cs, &r);
        assert!(text.retry);
        assert!(!d.join("upscaled").exists());
        // Closed: the job goes on (to the missing runner here) and lets go of the lock.
        drop(game);
        let r = finish(&mut start()).unwrap_err();
        assert!(!matches!(r, UpscaleFail::GameRunning), "{r:?}");
        setup::PlayingLock::acquire_exclusive(&d).unwrap();
    }

    /// The real job without a runner in the given directory fails clearly, without
    /// touching the data.
    #[test]
    fn job_without_runner_fails_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data");
        std::fs::create_dir_all(d.join("game")).unwrap();
        std::fs::write(d.join("game/import.json"), b"{}").unwrap();
        let mut job = UpscaleJob::start(
            d.clone(),
            RunnerSource::Dir(tmp.path().join("no runner here")),
            false,
            Box::new(|| {}),
        )
        .unwrap();
        let t0 = Instant::now();
        let r = loop {
            if let Some(r) = job.try_finish() {
                break r;
            }
            assert!(t0.elapsed().as_secs() < 20);
            std::thread::sleep(Duration::from_millis(5));
        };
        let r = r.unwrap_err();
        assert!(
            matches!(
                r,
                UpscaleFail::Error(
                    UpscaleError::RunnerMissing(_) | UpscaleError::UnsupportedPlatform
                )
            ),
            "{r:?}"
        );
        assert!(!fail_text(Lang::Cs, &r).retry);
        assert!(!d.join("upscaled").exists());
    }
}
