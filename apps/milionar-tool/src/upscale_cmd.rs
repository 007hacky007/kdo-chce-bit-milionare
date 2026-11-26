//! `upscale`: builds the upscaled art pack (local use only; the pack is
//! generated per user and never committed).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gamedata::GameData;
use upscale::pipeline::{self, Options, Phase, Progress};
use upscale::{FakeRunner, Runner, Upscaler};

/// The default runner directory: the first of [`upscale::runner::runner_dir_candidates`]
/// (shared with the game) holding the runner executable.
pub fn default_runner_dir(exe_dir: &Path) -> Result<PathBuf, String> {
    upscale::runner::find_runner_dir(exe_dir).map_err(|c| {
        let list: Vec<String> = c.iter().map(|d| format!("  {}", d.display())).collect();
        format!(
            "the Real-ESRGAN runner was not found; looked in:\n{}\n\
             Fetch it with tools/fetch-upscaler.sh (into tools/upscalers/) or pass --runner-dir <DIR>",
            list.join("\n")
        )
    })
}

/// How to run one `upscale`.
pub struct Opts<'a> {
    /// `None`: [`default_runner_dir`] of the running executable.
    pub runner_dir: Option<&'a Path>,
    pub fake: bool,
    pub keep_cache: bool,
    /// Testing: cancel once this many frames are done (as Ctrl+C would).
    pub cancel_after: Option<usize>,
    pub batch_size: Option<usize>,
}

fn mmss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}m{:02}s", s / 60, s % 60)
}

pub fn run(gd: &GameData, data_dir: &Path, o: &Opts<'_>) -> Result<String, String> {
    let real;
    let fake_runner = FakeRunner::new();
    let up: &dyn Upscaler = if o.fake {
        &fake_runner
    } else {
        let dir = match o.runner_dir {
            Some(d) => d.to_path_buf(),
            None => {
                let exe = std::env::current_exe()
                    .map_err(|e| format!("cannot find the tool executable: {e}"))?;
                // Without the Windows verbatim prefix: the runner cannot take it.
                let exe = exe
                    .canonicalize()
                    .map(|c| upscale::runner::plain_path(&c))
                    .unwrap_or(exe);
                default_runner_dir(exe.parent().unwrap_or(Path::new(".")))?
            }
        };
        real = Runner::locate(&dir).map_err(|e| format!("{e} (runner dir {})", dir.display()))?;
        &real
    };
    let mut opts = Options {
        keep_cache: o.keep_cache,
        ..Options::default()
    };
    if let Some(b) = o.batch_size {
        opts.batch_size = b.max(1);
    }
    let cancel = AtomicBool::new(false);
    let mut last = None;
    let mut progress = |p: &Progress| {
        // Only the upscale phase is printed (and counted for --cancel-after).
        if p.phase != Phase::Upscale {
            return;
        }
        if o.cancel_after.is_some_and(|n| p.done >= n) {
            cancel.store(true, Ordering::Relaxed);
        }
        if last == Some(p.done) {
            return;
        }
        last = Some(p.done);
        let pct = p.done * 100 / p.total.max(1);
        let eta = p
            .eta
            .map(|e| format!(", eta {}", mmss(e)))
            .unwrap_or_default();
        let _ = writeln!(
            std::io::stdout().lock(),
            "{}/{} frames ({pct}%), {}{eta}",
            p.done,
            p.total,
            mmss(p.elapsed)
        );
    };
    let r = pipeline::run(gd, data_dir, up, &opts, &mut progress, &cancel)
        .map_err(|e| e.to_string())?;
    let path = upscale::pack_path(data_dir);
    Ok(if r.up_to_date {
        format!(
            "{} is up to date: {} frames, {} bytes",
            path.display(),
            r.frames - r.conflicts,
            r.pack_bytes
        )
    } else {
        format!(
            "wrote {}: {} frames of {} files ({} unique: {} upscaled, {} from cache, {} from the old pack; {} left out), {} bytes in {}",
            path.display(),
            r.frames - r.conflicts,
            r.files,
            r.unique,
            r.upscaled,
            r.reused,
            r.seeded,
            r.conflicts,
            r.pack_bytes,
            mmss(r.elapsed)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_runner_lists_the_places_looked_in() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("target").join("release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let err = default_runner_dir(&exe_dir).unwrap_err();
        assert!(
            err.contains("fetch-upscaler.sh") && err.contains("--runner-dir"),
            "{err}"
        );
        assert!(
            err.contains(&exe_dir.join("upscaler").display().to_string()),
            "{err}"
        );
        let repo = root
            .path()
            .join("tools/upscalers")
            .join(upscale::runner::RUNNER_DIR_NAME);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(upscale::runner::RUNNER_EXE), b"").unwrap();
        assert_eq!(default_runner_dir(&exe_dir).unwrap(), repo);
    }
}
