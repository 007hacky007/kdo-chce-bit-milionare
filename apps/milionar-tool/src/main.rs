//! Developer CLI over the game data crates. Not shipped to players.

mod coverage;
mod export;
mod screenshot;
mod upscale_cmd;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap::{Parser, Subcommand};
use formats::questions::decode_question_db;
use gamedata::GameData;

#[derive(Parser)]
#[command(name = "milionar-tool", about = "Developer tool for the Milionar port")]
struct Cli {
    /// Data directory holding `game/` (default: the per-user data directory).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Import an ISO, Setup.exe or installed game folder into the data dir.
    Import { source: PathBuf },
    /// Re-hash the imported game files against the release manifest.
    Verify,
    /// Decode every question, audio entry and Smacker frame and check counts.
    Coverage {
        #[arg(long)]
        json: bool,
    },
    /// Write each frame of a Smacker file as PNG plus frames.json (local use only).
    ExportFrames { name: String, out_dir: PathBuf },
    /// Print the records of a question database (local use only).
    Questions {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=3))]
        db: u8,
    },
    /// Run a replay script headless and save the frame presented at --tick as PNG
    /// (local use only: write outside the repository).
    Screenshot {
        /// Replay script (JSON, see engine::replay).
        #[arg(long)]
        script: PathBuf,
        /// Override the script's seed.
        #[arg(long)]
        seed: Option<u32>,
        /// Present index to capture; several as a comma list or repeated (then --out may
        /// contain {tick}).
        #[arg(long, required = true, value_delimiter = ',')]
        tick: Vec<u64>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=8))]
        scale: u32,
        /// One-tick silent sounds instead of the shipped audio (changes timing).
        #[arg(long)]
        silent: bool,
        /// Print the screen entries (present index, screen) of the run.
        #[arg(long)]
        log: bool,
    },
    /// Upscale every Smacker frame 3x into <data-dir>/upscaled/frames.upk with the
    /// pinned Real-ESRGAN runner (fetch it with tools/fetch-upscaler.sh). Resumable.
    Upscale {
        /// Directory holding the runner and models/ (default: upscalers/<runner> next to
        /// this executable, else tools/upscalers/<runner> of the repository it was built
        /// in; never the working directory).
        #[arg(long, value_name = "DIR")]
        runner_dir: Option<PathBuf>,
        /// Nearest-neighbour stand-in for the runner (tests). Needs an explicit
        /// --data-dir so it can never replace the cache or pack in the default one.
        #[arg(long, hide = true)]
        fake_runner: bool,
        /// Cancel once this many frames are done, as an interrupt would (tests).
        #[arg(long, hide = true, value_name = "N")]
        cancel_after: Option<usize>,
        /// Frames per runner invocation (tests).
        #[arg(long, hide = true, value_name = "N")]
        batch_size: Option<usize>,
        /// Keep upscaled/cache/ after the pack is written.
        #[arg(long)]
        keep_cache: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.data_dir.is_none()
        && matches!(
            cli.command,
            Command::Upscale {
                fake_runner: true,
                ..
            }
        )
    {
        eprintln!("error: --fake-runner needs an explicit --data-dir");
        return ExitCode::from(2);
    }
    let data_dir = cli.data_dir.unwrap_or_else(gamedata::default_data_dir);
    match run(cli.command, &data_dir) {
        Ok(code) => code,
        Err(Fail::BrokenPipe) => ExitCode::SUCCESS,
        Err(Fail::Msg(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}

/// Why a command stopped early.
enum Fail {
    /// stdout was closed by the reader (e.g. `| head`); not an error.
    BrokenPipe,
    Msg(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Fail {
        Fail::Msg(s)
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Fail {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            Fail::BrokenPipe
        } else {
            Fail::Msg(e.to_string())
        }
    }
}

fn run(command: Command, data_dir: &std::path::Path) -> Result<ExitCode, Fail> {
    let mut out = std::io::stdout().lock();
    match command {
        Command::Import { source } => {
            let src = setup::detect_source(&source).map_err(|e| Fail::Msg(e.to_string()))?;
            let mut last = None;
            let mut progress = |p: &setup::Progress| {
                let pct = p.done_bytes * 100 / p.total_bytes.max(1);
                if last != Some(pct) {
                    last = Some(pct);
                    let _ = writeln!(std::io::stdout().lock(), "{pct}%");
                }
            };
            let cancel = AtomicBool::new(false);
            let report = setup::import(&src, data_dir, &setup::TOPCD, &mut progress, &cancel)
                .map_err(|e| Fail::Msg(e.to_string()))?;
            writeln!(
                out,
                "imported {} files, {} bytes from {} {}",
                report.files,
                report.bytes,
                report.source.kind(),
                report.source.path().display()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify => {
            setup::verify(data_dir, &setup::TOPCD).map_err(|e| Fail::Msg(e.to_string()))?;
            writeln!(out, "verified {} files", setup::TOPCD.files.len())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Coverage { json } => {
            let gd = open(data_dir)?;
            let report = coverage::run(&gd);
            if json {
                writeln!(out, "{}", report.to_json())?;
            } else {
                write!(out, "{}", report.to_text())?;
            }
            let problems = report.problems();
            for p in &problems {
                eprintln!("coverage: {p}");
            }
            Ok(if problems.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::ExportFrames { name, out_dir } => {
            let gd = open(data_dir)?;
            let n = export::export_frames(&gd, &name, &out_dir)?;
            writeln!(out, "wrote {n} frames to {}", out_dir.display())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Questions { db } => {
            let gd = open(data_dir)?;
            let bytes = gd
                .read(&format!("Db/Mill_{db}.dat"))
                .map_err(|e| Fail::Msg(e.to_string()))?;
            for q in decode_question_db(&bytes).map_err(|e| Fail::Msg(e.to_string()))? {
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    q.id, q.text, q.correct, q.wrong[0], q.wrong[1], q.wrong[2], q.voice_id
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Screenshot {
            script,
            seed,
            tick,
            out: path,
            scale,
            silent,
            log,
        } => {
            let o = screenshot::Options {
                script,
                seed,
                ticks: tick,
                out: path,
                scale,
                silent,
            };
            let r = screenshot::run(data_dir, &o)?;
            if log {
                for (t, s) in &r.log {
                    writeln!(out, "{t}\t{s}")?;
                }
            }
            for p in &r.written {
                writeln!(out, "wrote {}", p.display())?;
            }
            if !r.missing.is_empty() {
                return Err(Fail::Msg(format!(
                    "the run ended after {} presents; ticks {:?} not reached",
                    r.presents, r.missing
                )));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Upscale {
            runner_dir,
            fake_runner,
            keep_cache,
            cancel_after,
            batch_size,
        } => {
            let gd = open(data_dir)?;
            let o = upscale_cmd::Opts {
                runner_dir: runner_dir.as_deref(),
                fake: fake_runner,
                keep_cache,
                cancel_after,
                batch_size,
            };
            let r = upscale_cmd::run(&gd, data_dir, &o)?;
            writeln!(out, "{r}")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn open(data_dir: &std::path::Path) -> Result<GameData, String> {
    GameData::open(data_dir).map_err(|e| e.to_string())
}
