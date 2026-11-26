//! Turns an imported game into `<data_dir>/upscaled/frames.upk`.
//!
//! 1. Scan: decode every frame of every `.smk`, key it by (file, 1-based
//!    frame) and its [`content_hash`]. Identical frames (the four
//!    `Ans*Selc.smk`, repeats inside a file) are one unique frame.
//! 2. If a pack for this import, these runner params and exactly these keys
//!    and hashes already exists, stop (up to date).
//! 3. Unique frames already in the resume cache (`upscaled/cache/`) are
//!    skipped. Frames an existing pack (for example one made before a
//!    re-import) holds for the same content hash and blit, made by the same
//!    runner, params and pipeline version, are copied into the cache
//!    ("seeded"; the payload checksum is verified). The rest are decoded again file by file and handed to the
//!    upscaler in batches: PNG RGB with hidden pixels filled
//!    ([`alpha::fill_hidden`]), runner, binary 3x mask
//!    ([`alpha::apply_mask`]), encode, [`FrameCache::put`]. Every finished
//!    frame is a complete cache record, so cancelling or killing the process
//!    loses at most the batch in flight.
//! 4. The pack is assembled from the cache in a staging file and renamed
//!    into place. Only then is the cache deleted (kept with
//!    [`Options::keep_cache`]).
//!
//! Progress comes in three [`Phase`]s (scan, upscale, assemble), each with
//! its own done/total that only grows, so a bar can follow every phase.
//!
//! The cache carries a stamp (`cache/stamp.txt`) of the runner params,
//! codec, quality and pipeline version; on a mismatch its records are
//! discarded so frames from another upscaler are never mixed in.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use formats::smacker::Smacker;
use gamedata::GameData;

use crate::alpha::{self, BlitMode};
use crate::cache::{EncodedFrame, FrameCache};
use crate::key::{ContentHash, FrameKey, content_hash};
use crate::pack::{Codec, JPEG_QUALITY, PackHeader, PackReader, PackWriter, RunnerParams, encode};
use crate::runner::{Result, UpscaleError, Upscaler};

/// Bump when the frame preparation (fill, mask, blit rule) changes; old
/// cache records are then discarded.
pub const PIPELINE_VERSION: u32 = 1;
/// Frames per runner invocation.
pub const DEFAULT_BATCH: usize = 64;

#[derive(Debug, Clone)]
pub struct Options {
    pub batch_size: usize,
    /// Keep `upscaled/cache/` after a successful promote (default: delete).
    pub keep_cache: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            batch_size: DEFAULT_BATCH,
            keep_cache: false,
        }
    }
}

/// The steps of a run, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Decoding and hashing every frame; done/total count files.
    Scan,
    /// Seeding from an old pack and upscaling; done/total count unique
    /// frames (cached before this run, seeded or finished now).
    Upscale,
    /// Writing the pack from the cache; done/total count pack entries.
    Assemble,
}

/// Where a run is. Within a phase `done` never decreases and never exceeds
/// `total`; phases follow in order. (A cache record that fails its checksum
/// during assembly sends the run back to [`Phase::Upscale`] for that frame;
/// that only happens on a damaged disk.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    /// Since the start of the run.
    pub elapsed: Duration,
    /// Time left in this phase from this run's rate in it; `None` until
    /// something was done.
    pub eta: Option<Duration>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub files: usize,
    pub frames: usize,
    pub unique: usize,
    /// Unique frames upscaled in this run.
    pub upscaled: usize,
    /// Unique frames taken from the cache.
    pub reused: usize,
    /// Unique frames copied from an existing pack into the cache.
    pub seeded: usize,
    /// Frames left out of the pack because the same content is drawn
    /// opaque elsewhere (the runtime falls back to the original pixels).
    pub conflicts: usize,
    pub pack_bytes: u64,
    /// The existing pack already matched; nothing was done.
    pub up_to_date: bool,
    pub elapsed: Duration,
}

/// One decoded source frame.
pub struct SourceFrame<'a> {
    /// 1-based.
    pub frame: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
    pub palette: &'a [[u8; 3]; 256],
}

/// Where the frames come from: the game import, or synthetic frames in tests.
pub trait FrameSource {
    /// Normalised file names ([`FrameKey`] form), sorted.
    fn files(&self) -> Vec<String>;
    /// Calls `f` for every frame of `file` in order.
    fn frames(&self, file: &str, f: &mut dyn FnMut(&SourceFrame<'_>) -> Result<()>) -> Result<()>;
}

/// Every `.smk` under `game/`.
pub struct GameSource {
    files: Vec<(String, PathBuf)>,
}

impl GameSource {
    pub fn new(game: &GameData) -> GameSource {
        let mut files: Vec<(String, PathBuf)> = game
            .files()
            .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("smk")))
            .map(|p| {
                let rel = p.strip_prefix(game.game_dir()).unwrap_or(p);
                let name: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                (FrameKey::new(&name.join("/"), 1).file, p.to_path_buf())
            })
            .collect();
        files.sort();
        GameSource { files }
    }
}

impl FrameSource for GameSource {
    fn files(&self) -> Vec<String> {
        self.files.iter().map(|(n, _)| n.clone()).collect()
    }

    fn frames(&self, file: &str, f: &mut dyn FnMut(&SourceFrame<'_>) -> Result<()>) -> Result<()> {
        let Some((_, path)) = self.files.iter().find(|(n, _)| n == file) else {
            return Err(UpscaleError::Game(format!("{file}: not found")));
        };
        let game = |e: &dyn std::fmt::Display| UpscaleError::Game(format!("{file}: {e}"));
        let bytes = std::fs::read(path).map_err(|e| game(&e))?;
        let smk = Smacker::parse(bytes).map_err(|e| game(&e))?;
        let (width, height) = (smk.header().width, smk.header().height);
        let mut dec = smk.decoder();
        let mut frame = 0u32;
        while let Some(fr) = dec.next_frame().map_err(|e| game(&e))? {
            frame += 1;
            if fr.pixels.len() != width as usize * height as usize {
                return Err(game(&"frame size does not match the header"));
            }
            f(&SourceFrame {
                frame,
                width,
                height,
                pixels: fr.pixels,
                palette: fr.palette,
            })?;
        }
        Ok(())
    }
}

/// Runs the pipeline over the game import in `data_dir`.
pub fn run(
    game: &GameData,
    data_dir: &Path,
    up: &dyn Upscaler,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<Report> {
    run_source(&GameSource::new(game), data_dir, up, opts, progress, cancel)
}

/// One unique frame still to make: its hash, blit and the file it first appears in.
type Unique = (ContentHash, BlitMode, String);

struct Scan {
    /// Keys in pack order with their hash and the file's blit mode;
    /// `false` = left out (conflict).
    keys: Vec<(FrameKey, ContentHash, BlitMode, bool)>,
    /// Unique hashes in first-occurrence order with their mode and the
    /// file of the first occurrence.
    unique: Vec<Unique>,
    conflicts: usize,
    files: usize,
}

/// Reports the progress of one phase with an estimate from its own rate.
struct Meter<'a> {
    phase: Phase,
    run_started: Instant,
    started: Instant,
    /// `done` at the start of the phase (work from earlier runs).
    base: usize,
    total: usize,
    progress: &'a mut dyn FnMut(&Progress),
}

impl<'a> Meter<'a> {
    fn new(
        phase: Phase,
        run_started: Instant,
        base: usize,
        total: usize,
        progress: &'a mut dyn FnMut(&Progress),
    ) -> Meter<'a> {
        Meter {
            phase,
            run_started,
            started: Instant::now(),
            base,
            total,
            progress,
        }
    }

    /// Estimates from here on count only work done after `done`.
    fn rebase(&mut self, done: usize) {
        self.base = done;
        self.started = Instant::now();
    }

    fn report(&mut self, done: usize) {
        let done = done.min(self.total);
        let now = done.saturating_sub(self.base);
        let eta = (now > 0).then(|| {
            self.started
                .elapsed()
                .mul_f64((self.total - done) as f64 / now as f64)
        });
        (self.progress)(&Progress {
            phase: self.phase,
            done,
            total: self.total,
            elapsed: self.run_started.elapsed(),
            eta,
        });
    }
}

fn scan(src: &dyn FrameSource, cancel: &AtomicBool, meter: &mut Meter<'_>) -> Result<Scan> {
    let mut raw: Vec<(FrameKey, ContentHash, BlitMode)> = Vec::new();
    let mut modes: HashMap<ContentHash, (usize, BlitMode)> = HashMap::new();
    let mut order: Vec<(ContentHash, String)> = Vec::new();
    let files = src.files();
    meter.total = files.len();
    meter.report(0);
    for (i, file) in files.iter().enumerate() {
        let mode = alpha::blit_mode(file);
        src.frames(file, &mut |f| {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpscaleError::Cancelled);
            }
            let h = content_hash(f.pixels, f.palette, f.width, f.height);
            raw.push((FrameKey::new(file, f.frame), h, mode));
            let e = modes.entry(h).or_insert_with(|| {
                order.push((h, file.clone()));
                (order.len() - 1, mode)
            });
            // Opaque wins: its RGB under index 0 is the visible picture.
            if mode == BlitMode::Opaque {
                e.1 = BlitMode::Opaque;
            }
            Ok(())
        })?;
        meter.report(i + 1);
    }
    let mut conflicts = 0;
    let keys = raw
        .into_iter()
        .map(|(k, h, m)| {
            let keep = modes[&h].1 == m;
            conflicts += usize::from(!keep);
            (k, h, m, keep)
        })
        .collect();
    let unique = order
        .into_iter()
        .map(|(h, file)| (h, modes[&h].1, file))
        .collect();
    Ok(Scan {
        keys,
        unique,
        conflicts,
        files: files.len(),
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn stamp(p: &RunnerParams) -> String {
    format!(
        "milionar upscale cache\npipeline {PIPELINE_VERSION}\ncodec {} quality {JPEG_QUALITY}\nrunner {}\nparam {}\nbin {}\nmodel {} scale {} tile {} tta {} gpu {}\n",
        Codec::DEFAULT.id(),
        hex(&p.runner_sha256),
        hex(&p.model_param_sha256),
        hex(&p.model_bin_sha256),
        p.model_name,
        p.scale,
        p.tile,
        p.tta,
        p.gpu
    )
}

/// Opens the cache and discards its records unless the stamp matches.
fn open_cache(dir: &Path, params: &RunnerParams) -> Result<FrameCache> {
    let cache = FrameCache::open(dir)?;
    cache.sweep_temp()?;
    let want = stamp(params);
    let path = dir.join("stamp.txt");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str()) {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_name().to_string_lossy().ends_with(".bin") && e.file_type()?.is_file() {
                std::fs::remove_file(e.path())?;
            }
        }
        let mut tmp = tempfile::Builder::new()
            .prefix(".")
            .suffix(".tmp")
            .tempfile_in(dir)?;
        std::io::Write::write_all(&mut tmp, want.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path).map_err(|e| UpscaleError::Io(e.error))?;
    }
    Ok(cache)
}

/// True when the pack at `path` was made from this import by this upscaler
/// and pipeline version, and holds exactly the scanned keys with their
/// hashes and each file's blit mode.
fn pack_is_current(path: &Path, header: &PackHeader, scan: &Scan) -> bool {
    let Ok(r) = PackReader::open(path, &header.source_manifest_sha256) else {
        return false;
    };
    let keep = scan.keys.iter().filter(|k| k.3).count();
    r.header() == header
        && r.len() == keep
        && scan.keys.iter().filter(|k| k.3).all(|(k, h, m, _)| {
            r.entry(k).is_some_and(|e| e.content_hash == *h) && r.blit_mode(&k.file) == Some(*m)
        })
}

/// Free disk space a run still needs, out of `full` for a run from scratch: nothing
/// when the pack is current for this import, runner and pipeline version (the run
/// will only confirm it), otherwise `full` less the cache records made with `params`
/// that the run will reuse. An unreadable pack or cache counts as absent.
pub fn free_space_needed(data_dir: &Path, params: &RunnerParams, full: u64) -> u64 {
    if let Ok(manifest) = crate::manifest_hash(data_dir)
        && let Ok(r) = PackReader::open(&crate::pack_path(data_dir), &manifest)
        && r.header().params == *params
        && r.header().pipeline_version == PIPELINE_VERSION
    {
        return 0;
    }
    let dir = crate::cache_dir(data_dir);
    let stamped = std::fs::read_to_string(dir.join("stamp.txt")).ok() == Some(stamp(params));
    let reusable: u64 = if stamped {
        std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
                    .filter_map(|e| e.metadata().ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    } else {
        0
    };
    full.saturating_sub(reusable)
}

/// Removes `.work-*` directories left by killed runs.
fn sweep_work(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(".work-")
                && e.file_type().is_ok_and(|t| t.is_dir())
            {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// Name of the single-writer lock inside `<data_dir>/upscaled/`.
pub const LOCK_FILE: &str = ".upscale.lock";

/// Takes the exclusive upscale lock; released when the file is dropped.
/// The lock file is left in place (deleting it would race). It is a
/// separate lock from setup's `.import.lock`. std opens files
/// close-on-exec, so the runner child does not keep it.
fn lock(out_dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(UpscaleError::Busy(path)),
        Err(fs4::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Runs the pipeline over any frame source; see the module docs.
///
/// Holds `<data_dir>/upscaled/.upscale.lock` for the whole run (sweeps
/// included) and returns [`UpscaleError::Busy`] without touching anything
/// else when another run holds it.
pub fn run_source(
    src: &dyn FrameSource,
    data_dir: &Path,
    up: &dyn Upscaler,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<Report> {
    let started = Instant::now();
    let manifest = crate::manifest_hash(data_dir)?;
    let out_dir = data_dir.join("upscaled");
    let _lock = lock(&out_dir)?;
    let header = PackHeader {
        params: up.params(),
        source_manifest_sha256: manifest,
        pipeline_version: PIPELINE_VERSION,
    };
    let scan = {
        let mut m = Meter::new(Phase::Scan, started, 0, 0, progress);
        scan(src, cancel, &mut m)?
    };
    let mut report = Report {
        files: scan.files,
        frames: scan.keys.len(),
        unique: scan.unique.len(),
        conflicts: scan.conflicts,
        ..Report::default()
    };
    let pack_path = crate::pack_path(data_dir);
    let cache_dir = crate::cache_dir(data_dir);
    if pack_is_current(&pack_path, &header, &scan) {
        if !opts.keep_cache {
            remove_dir(&cache_dir)?;
        }
        report.up_to_date = true;
        report.pack_bytes = std::fs::metadata(&pack_path)?.len();
        report.elapsed = started.elapsed();
        return Ok(report);
    }
    sweep_work(&out_dir);
    let cache = open_cache(&cache_dir, &header.params)?;
    // A record can turn out invalid only when it is read back in full
    // (`get` then deletes it), so assembly may send frames back once more.
    for attempt in 0..3 {
        let pending: Vec<&Unique> = scan
            .unique
            .iter()
            .filter(|(h, m, _)| !cache.contains(h, *m))
            .collect();
        let base = scan.unique.len() - pending.len();
        if attempt == 0 {
            report.reused = base;
        }
        let mut meter = Meter::new(Phase::Upscale, started, base, scan.unique.len(), progress);
        meter.report(base);
        let pending = if attempt == 0 && !pending.is_empty() {
            let (seeded, rest) = seed(&pack_path, &header, &cache, pending, &mut meter, cancel)?;
            report.seeded = seeded;
            // The estimate follows the upscaling rate, not the much faster copy.
            meter.rebase(base + seeded);
            rest
        } else {
            pending
        };
        if !pending.is_empty() {
            report.upscaled += upscale_pending(
                src, &out_dir, &cache, up, &pending, opts, &mut meter, cancel,
            )?;
        }
        let keep = scan.keys.iter().filter(|k| k.3).count();
        let mut meter = Meter::new(Phase::Assemble, started, 0, keep, progress);
        match assemble(&pack_path, &header, &scan, &cache, &mut meter, cancel)? {
            true => {
                report.pack_bytes = std::fs::metadata(&pack_path)?.len();
                if !opts.keep_cache {
                    remove_dir(&cache_dir)?;
                }
                report.elapsed = started.elapsed();
                return Ok(report);
            }
            false => continue,
        }
    }
    Err(UpscaleError::Pack(crate::PackError::Invalid(
        "cache records keep failing their checksum; check the disk".into(),
    )))
}

/// Copies the frames of `pending` that an existing pack at `path` holds
/// into the cache, when that pack was made by the same runner, params and
/// pipeline version (from any import: the content hash and blit mode pin
/// the source frame, so the upscaled result is the same). Returns how many
/// were seeded and the frames still to upscale. A pack that cannot be read
/// seeds nothing; a payload that fails its checksum is skipped.
fn seed<'s>(
    path: &Path,
    header: &PackHeader,
    cache: &FrameCache,
    pending: Vec<&'s Unique>,
    meter: &mut Meter<'_>,
    cancel: &AtomicBool,
) -> Result<(usize, Vec<&'s Unique>)> {
    let Ok(old) = PackReader::open_any_import(path) else {
        return Ok((0, pending));
    };
    if old.header().params != header.params || old.header().pipeline_version != PIPELINE_VERSION {
        return Ok((0, pending));
    }
    let mut by_hash: HashMap<(ContentHash, BlitMode), &crate::Entry> = HashMap::new();
    for k in old.keys() {
        if let (Some(e), Some(m)) = (old.entry(k), old.blit_mode(&k.file)) {
            by_hash.entry((e.content_hash, m)).or_insert(e);
        }
    }
    let mut done = meter.base;
    let mut rest = Vec::new();
    for item in pending {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpscaleError::Cancelled);
        }
        let (h, m, _) = item;
        let payload = by_hash
            .get(&(*h, *m))
            .and_then(|e| old.read_payload(e).ok().map(|p| (e, p)));
        match payload {
            Some((e, payload)) => {
                cache.put(
                    h,
                    &EncodedFrame {
                        mode: *m,
                        width: e.width,
                        height: e.height,
                        codec: e.codec,
                        payload,
                    },
                )?;
                done += 1;
                if done.is_multiple_of(64) {
                    meter.report(done);
                }
            }
            None => rest.push(item),
        }
    }
    meter.report(done);
    Ok((done - meter.base, rest))
}

fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Writes the pack from the cache; `false` when a record was missing
/// (the staging file is discarded and the frame must be redone).
fn assemble(
    path: &Path,
    header: &PackHeader,
    scan: &Scan,
    cache: &FrameCache,
    meter: &mut Meter<'_>,
    cancel: &AtomicBool,
) -> Result<bool> {
    let mut w = PackWriter::create(path, header, Codec::DEFAULT)?;
    let mut ok = true;
    let mut done = 0usize;
    meter.report(0);
    for (key, hash, mode, keep) in &scan.keys {
        if !keep {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(UpscaleError::Cancelled);
        }
        match cache.get(hash, *mode) {
            Some(enc) => w.add_cached(key, *hash, &enc)?,
            None => ok = false,
        }
        done += 1;
        if done.is_multiple_of(64) {
            meter.report(done);
        }
    }
    if ok {
        w.finish()?;
        meter.report(done);
    }
    Ok(ok)
}

struct Item {
    hash: ContentHash,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    mode: BlitMode,
}

struct Batcher<'a, 'm> {
    work_in: PathBuf,
    work_out: PathBuf,
    cache: &'a FrameCache,
    up: &'a dyn Upscaler,
    items: Vec<Item>,
    done: usize,
    meter: &'a mut Meter<'m>,
    cancel: &'a AtomicBool,
}

impl Batcher<'_, '_> {
    fn flush(&mut self) -> Result<()> {
        if self.items.is_empty() {
            return Ok(());
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(UpscaleError::Cancelled);
        }
        self.up
            .upscale_dir(&self.work_in, &self.work_out, self.cancel)?;
        let items = std::mem::take(&mut self.items);
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(items.len());
        let chunk = items.len().div_ceil(threads);
        let (cache, out, scale) = (self.cache, &self.work_out, self.up.params().scale);
        std::thread::scope(|s| {
            let handles: Vec<_> = items
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || part.iter().try_for_each(|it| finish(it, out, scale, cache)))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(UpscaleError::Game("encoder thread panicked".into()))
                    })
                })
                .collect::<Result<Vec<()>>>()
        })?;
        for d in [&self.work_in, &self.work_out] {
            for e in std::fs::read_dir(d)? {
                std::fs::remove_file(e?.path())?;
            }
        }
        self.done += items.len();
        self.meter.report(self.done);
        Ok(())
    }
}

/// Reads one runner output, masks, encodes and caches it.
fn finish(it: &Item, out: &Path, scale: u32, cache: &FrameCache) -> Result<()> {
    let name = format!("{}.png", hex(&it.hash));
    let (w, h, rgb) = crate::img::read_png_rgb(&out.join(&name))?;
    if (w, h) != (it.width * scale, it.height * scale) {
        return Err(UpscaleError::BadOutput {
            file: name,
            reason: format!(
                "{w}x{h}, expected {}x{}",
                it.width * scale,
                it.height * scale
            ),
        });
    }
    let frame = alpha::apply_mask(&rgb, &it.pixels, it.width, it.height, scale, it.mode)
        .ok_or_else(|| UpscaleError::BadOutput {
            file: name.clone(),
            reason: "size mismatch".into(),
        })?;
    let payload = encode(Codec::DEFAULT, &frame)?;
    cache.put(
        &it.hash,
        &EncodedFrame {
            mode: it.mode,
            width: frame.width,
            height: frame.height,
            codec: Codec::DEFAULT,
            payload,
        },
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upscale_pending(
    src: &dyn FrameSource,
    out_dir: &Path,
    cache: &FrameCache,
    up: &dyn Upscaler,
    pending: &[&Unique],
    opts: &Options,
    meter: &mut Meter<'_>,
    cancel: &AtomicBool,
) -> Result<usize> {
    let work = tempfile::Builder::new()
        .prefix(".work-")
        .tempdir_in(out_dir)?;
    let work_in = work.path().join("in");
    let work_out = work.path().join("out");
    std::fs::create_dir_all(&work_in)?;
    std::fs::create_dir_all(&work_out)?;
    let mut todo: HashMap<ContentHash, BlitMode> =
        pending.iter().map(|(h, m, _)| (*h, *m)).collect();
    let files: HashSet<&str> = pending.iter().map(|(_, _, f)| f.as_str()).collect();
    let n = pending.len();
    let mut b = Batcher {
        work_in,
        work_out,
        cache,
        up,
        items: Vec::new(),
        done: meter.total - n,
        meter,
        cancel,
    };
    let batch = opts.batch_size.max(1);
    for file in src.files() {
        if !files.contains(file.as_str()) {
            continue;
        }
        src.frames(&file, &mut |f| {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpscaleError::Cancelled);
            }
            let h = content_hash(f.pixels, f.palette, f.width, f.height);
            let Some(mode) = todo.remove(&h) else {
                return Ok(());
            };
            let rgb = alpha::fill_hidden(f.pixels, f.palette, f.width, f.height, mode);
            crate::img::write_png_rgb(
                &b.work_in.join(format!("{}.png", hex(&h))),
                f.width,
                f.height,
                &rgb,
            )?;
            b.items.push(Item {
                hash: h,
                width: f.width,
                height: f.height,
                pixels: f.pixels.to_vec(),
                mode,
            });
            if b.items.len() >= batch {
                b.flush()?;
            }
            Ok(())
        })?;
    }
    b.flush()?;
    if !todo.is_empty() {
        return Err(UpscaleError::Game(format!(
            "{} frames changed between the two passes over the game files",
            todo.len()
        )));
    }
    Ok(n)
}
