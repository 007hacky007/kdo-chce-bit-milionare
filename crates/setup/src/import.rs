//! Transactional import into `<data_dir>/game`.
//!
//! Files are copied and hashed into `<data_dir>/.staging-<pid>/game`. Only
//! when every file matches the manifest is the previous `game` moved to
//! `.old-<pid>/game` and the staged tree renamed into place, so a failed or
//! cancelled import leaves the previous data untouched.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use formats::udf::{UdfFile, UdfImage};
use sha2::{Digest, Sha256};

use crate::manifest::hex;
use crate::source::{find_path, install_root};
use crate::{ImportError, Manifest, ManifestEntry, Source};

const CHUNK: usize = 1 << 20;
const SPACE_MARGIN: u64 = 64 << 20;
const SETUP_NAME: &str = "Setup.exe";
/// Held exclusively for the whole import so concurrent imports into the same
/// data dir cannot delete each other's staging or `.old-*` directories.
const LOCK_NAME: &str = ".import.lock";
/// Held shared by every running game ([`PlayingLock`]) and exclusively by an import, so
/// an import never swaps `game/` under a running game.
const PLAYING_LOCK_NAME: &str = ".playing.lock";

/// Import progress, reported after every chunk (1 MiB) and every file.
///
/// For `Iso` and `SetupExe` sources `total_bytes` is the size of
/// `Setup.exe`, which is read once end to end; for `Installed` it is the sum
/// of the manifest file sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Manifest path of the file being copied (or next to be copied).
    pub current: &'static str,
}

/// Summary of a successful import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub source: Source,
    pub files: usize,
    pub bytes: u64,
}

/// Copies and verifies all manifest files from `src` into `data_dir/game`.
///
/// Leftovers of an interrupted earlier import are cleaned up first: stale
/// `.staging-*` directories are removed and, when `game` is missing, a
/// `.old-*/game` is restored. On any error or cancellation the previous
/// `game` directory is kept as it was. Returns `Busy` when another import
/// holds the data dir lock and `GameRunning` while a game holds a [`PlayingLock`].
pub fn import(
    src: &Source,
    data_dir: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<ImportReport, ImportError> {
    fs::create_dir_all(data_dir).map_err(ImportError::io(data_dir))?;
    let _lock = lock_data_dir(data_dir)?;
    let _playing = lock_playing_exclusive(data_dir)?;
    recover_locked(data_dir)?;
    let pid = std::process::id();
    let staging = data_dir.join(format!(".staging-{pid}"));
    let result = stage_and_promote(src, data_dir, &staging, pid, manifest, progress, cancel);
    // On success only an empty directory is left; a failure to remove it
    // is harmless because the next import deletes stale `.staging-*`.
    let _ = fs::remove_dir_all(&staging);
    result?;
    Ok(ImportReport {
        source: src.clone(),
        files: manifest.files.len(),
        bytes: manifest.total_size(),
    })
}

fn stage_and_promote(
    src: &Source,
    data_dir: &Path,
    staging: &Path,
    pid: u32,
    manifest: &Manifest,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<(), ImportError> {
    check_replaceable(&data_dir.join("game"))?;
    // Open and sanity-check the source before anything is staged, so a
    // truncated image fails at once instead of part way through the copy.
    let input = match src {
        Source::Iso(iso) => Input::Setup(open_iso(iso)?),
        Source::SetupExe(exe) => Input::Setup(SetupReader::File {
            path: exe.clone(),
            file: File::open(exe).map_err(ImportError::io(exe))?,
        }),
        Source::Installed(dir) => Input::Installed(dir),
    };
    let needed = manifest.total_size() + SPACE_MARGIN;
    let available = fs4::available_space(data_dir).map_err(ImportError::io(data_dir))?;
    if available < needed {
        return Err(ImportError::NoSpace { needed, available });
    }
    let game = staging.join("game");
    fs::create_dir_all(&game).map_err(ImportError::io(&game))?;
    match input {
        Input::Setup(mut r) => copy_from_setup(&mut r, &game, manifest, progress, cancel)?,
        Input::Installed(dir) => copy_installed(dir, &game, manifest, progress, cancel)?,
    }
    write_import_json(&game, src, manifest)?;
    promote(data_dir, &game, pid)
}

enum Input<'a> {
    Setup(SetupReader),
    Installed(&'a Path),
}

/// Opens the UDF image, locates `Setup.exe` and checks that the image file
/// is long enough to hold all of it.
fn open_iso(iso: &Path) -> Result<SetupReader, ImportError> {
    let file = File::open(iso).map_err(ImportError::io(iso))?;
    let len = file.metadata().map_err(ImportError::io(iso))?.len();
    let mut img = UdfImage::open(file).map_err(ImportError::image(iso))?;
    let exe = img
        .find(SETUP_NAME)
        .map_err(ImportError::image(iso))?
        .ok_or_else(|| ImportError::Missing(SETUP_NAME.into()))?;
    let expected = exe.recorded_end();
    if expected > len {
        return Err(ImportError::Incomplete {
            path: iso.to_path_buf(),
            actual: len,
            expected,
        });
    }
    Ok(SetupReader::Udf {
        path: iso.to_path_buf(),
        img,
        exe,
    })
}

/// An existing `game` may only be replaced when an earlier import created
/// it (it holds `import.json`) or it is an empty directory.
fn check_replaceable(game: &Path) -> Result<(), ImportError> {
    let meta = match fs::symlink_metadata(game) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(ImportError::Io {
                path: game.to_path_buf(),
                source: e,
            });
        }
    };
    if meta.is_dir() {
        if game.join("import.json").is_file() {
            return Ok(());
        }
        let mut entries = fs::read_dir(game).map_err(ImportError::io(game))?;
        if entries.next().is_none() {
            return Ok(());
        }
    }
    Err(ImportError::ForeignGameDir(game.to_path_buf()))
}

fn open_lock_file(path: &Path) -> Result<File, ImportError> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(ImportError::io(path))
}

/// Takes the exclusive import lock; it is released when the file is dropped.
/// The lock file itself is left in place (deleting it would race).
fn lock_data_dir(data_dir: &Path) -> Result<File, ImportError> {
    let path = data_dir.join(LOCK_NAME);
    let file = open_lock_file(&path)?;
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(ImportError::Busy(data_dir.to_path_buf())),
        Err(fs4::TryLockError::Error(e)) => Err(ImportError::Io { path, source: e }),
    }
}

/// The import's side of the playing lock: exclusive, so it fails while any game runs.
fn lock_playing_exclusive(data_dir: &Path) -> Result<File, ImportError> {
    let path = data_dir.join(PLAYING_LOCK_NAME);
    let file = open_lock_file(&path)?;
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(ImportError::GameRunning(data_dir.to_path_buf())),
        Err(fs4::TryLockError::Error(e)) => Err(ImportError::Io { path, source: e }),
    }
}

/// A shared lock on `<data_dir>/.playing.lock`, held by a running game for as long as
/// it reads `game/`. Any number of games can hold it at once; [`import`] takes the same
/// file exclusively and fails with [`ImportError::GameRunning`] while one is held.
///
/// Locks are per open file (`flock` on Unix, `LockFileEx` on Windows), so a game and an
/// import in the same process exclude each other too. Released on drop.
#[derive(Debug)]
pub struct PlayingLock {
    _file: File,
}

impl PlayingLock {
    /// Takes the shared lock, creating `data_dir` when needed. `Busy` while an import
    /// holds it exclusively.
    pub fn acquire(data_dir: &Path) -> Result<PlayingLock, ImportError> {
        fs::create_dir_all(data_dir).map_err(ImportError::io(data_dir))?;
        let path = data_dir.join(PLAYING_LOCK_NAME);
        let file = open_lock_file(&path)?;
        match fs4::FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(PlayingLock { _file: file }),
            Err(fs4::TryLockError::WouldBlock) => Err(ImportError::Busy(data_dir.to_path_buf())),
            Err(fs4::TryLockError::Error(e)) => Err(ImportError::Io { path, source: e }),
        }
    }

    /// Takes the lock exclusively, like an import does: for the upscale step, which
    /// reads `game/` for minutes and replaces the pack a game may hold open.
    /// `GameRunning` while a game (or an import) holds it.
    pub fn acquire_exclusive(data_dir: &Path) -> Result<PlayingLock, ImportError> {
        fs::create_dir_all(data_dir).map_err(ImportError::io(data_dir))?;
        lock_playing_exclusive(data_dir).map(|file| PlayingLock { _file: file })
    }
}

/// Cleans up after an interrupted import: removes `.staging-*` leftovers and
/// restores `.old-*/game` when `game` is missing (a crash between the two
/// promote renames). Meant to run at app startup; `import` does it too.
///
/// Takes the import lock, so it returns `Busy` while an import is running.
/// A missing `data_dir` is not an error (nothing to recover).
pub fn recover(data_dir: &Path) -> Result<(), ImportError> {
    if !data_dir.is_dir() {
        return Ok(());
    }
    let _lock = lock_data_dir(data_dir)?;
    recover_locked(data_dir)
}

fn recover_locked(data_dir: &Path) -> Result<(), ImportError> {
    let mut olds = Vec::new();
    for entry in fs::read_dir(data_dir).map_err(ImportError::io(data_dir))? {
        let entry = entry.map_err(ImportError::io(data_dir))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with(".staging-") {
            remove_any(&path)?;
        } else if name.starts_with(".old-") {
            olds.push(path);
        }
    }
    olds.sort();
    let game = data_dir.join("game");
    if fs::symlink_metadata(&game).is_err()
        && let Some(old) = olds.iter().rev().find(|o| o.join("game").is_dir())
    {
        fs::rename(old.join("game"), &game).map_err(ImportError::io(&game))?;
    }
    for old in &olds {
        remove_any(old)?;
    }
    Ok(())
}

fn remove_any(path: &Path) -> Result<(), ImportError> {
    let r = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    r.map_err(ImportError::io(path))
}

/// Swaps the staged `game` into place, keeping the previous one until the
/// new one is in.
fn promote(data_dir: &Path, staged: &Path, pid: u32) -> Result<(), ImportError> {
    let game = data_dir.join("game");
    let old = data_dir.join(format!(".old-{pid}"));
    let old_game = old.join("game");
    check_replaceable(&game)?;
    let had_previous = fs::symlink_metadata(&game).is_ok();
    if had_previous {
        fs::create_dir_all(&old).map_err(ImportError::io(&old))?;
        if let Err(e) = fs::rename(&game, &old_game) {
            let _ = fs::remove_dir(&old);
            return Err(ImportError::Io {
                path: game,
                source: e,
            });
        }
    }
    if let Err(e) = fs::rename(staged, &game) {
        if had_previous && fs::rename(&old_game, &game).is_ok() {
            let _ = fs::remove_dir(&old);
        }
        return Err(ImportError::Io {
            path: game,
            source: e,
        });
    }
    if had_previous {
        // A failure here only leaves `.old-<pid>`, removed by the next import.
        let _ = fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Progress reporting and cancellation shared by both copy paths.
struct Tracker<'a> {
    done: u64,
    total: u64,
    progress: &'a mut dyn FnMut(&Progress),
    cancel: &'a AtomicBool,
}

impl Tracker<'_> {
    fn check_cancel(&self) -> Result<(), ImportError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(ImportError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn advance(&mut self, n: u64, current: &'static str) -> Result<(), ImportError> {
        self.done += n;
        (self.progress)(&Progress {
            done_bytes: self.done,
            total_bytes: self.total,
            current,
        });
        self.check_cancel()
    }
}

enum SetupReader {
    File {
        path: PathBuf,
        file: File,
    },
    Udf {
        path: PathBuf,
        img: UdfImage<File>,
        exe: UdfFile,
    },
}

impl SetupReader {
    fn len(&self) -> Result<u64, ImportError> {
        match self {
            SetupReader::File { path, file } => file
                .metadata()
                .map(|m| m.len())
                .map_err(ImportError::io(path)),
            SetupReader::Udf { exe, .. } => Ok(exe.size),
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ImportError> {
        match self {
            SetupReader::File { path, file } => file
                .seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(buf))
                .map_err(ImportError::io(path.as_path())),
            SetupReader::Udf { path, img, exe } => img
                .read_at(exe, offset, buf)
                .map_err(ImportError::image(path.as_path())),
        }
    }
}

fn setup_mismatch(expected: String, actual: String) -> ImportError {
    ImportError::Mismatch {
        file: SETUP_NAME.into(),
        expected,
        actual,
    }
}

/// Reads `Setup.exe` once from start to end, hashing all of it and writing
/// each manifest range to its file(s) on the way.
fn copy_from_setup(
    reader: &mut SetupReader,
    game: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<(), ImportError> {
    let size = reader.len()?;
    if size != manifest.setup_size {
        return Err(setup_mismatch(
            format!("{} bytes", manifest.setup_size),
            format!("{size} bytes"),
        ));
    }
    let mut t = Tracker {
        done: 0,
        total: size,
        progress,
        cancel,
    };
    let mut whole = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut pos = 0u64;
    let files = manifest.files;
    let mut i = 0;
    while i < files.len() {
        t.check_cancel()?;
        let first = files[i];
        let group_len = files[i..]
            .iter()
            .take_while(|f| f.setup_offset == first.setup_offset && f.size == first.size)
            .count();
        let group = &files[i..i + group_len];
        let end = first
            .setup_offset
            .checked_add(first.size)
            .filter(|&e| first.setup_offset >= pos && e <= size)
            .ok_or_else(|| ImportError::Io {
                path: PathBuf::from(first.path),
                source: io::Error::other("manifest ranges overlap or exceed Setup.exe"),
            })?;
        // Bytes between files only feed the whole-file hash.
        pump(
            reader,
            &mut buf,
            pos,
            first.setup_offset,
            &mut t,
            first.path,
            &mut |c| {
                whole.update(c);
                Ok(())
            },
        )?;
        let mut outs = group
            .iter()
            .map(|e| create_out(game, e))
            .collect::<Result<Vec<_>, _>>()?;
        let mut h = Sha256::new();
        pump(
            reader,
            &mut buf,
            first.setup_offset,
            end,
            &mut t,
            first.path,
            &mut |c| {
                whole.update(c);
                h.update(c);
                for (path, w) in &mut outs {
                    w.write_all(c).map_err(ImportError::io(path.as_path()))?;
                }
                Ok(())
            },
        )?;
        for (path, w) in outs {
            w.sync_all().map_err(ImportError::io(&path))?;
        }
        verify(&first, h.finalize().into())?;
        pos = end;
        i += group_len;
    }
    let last = files.last().map_or(SETUP_NAME, |f| f.path);
    pump(reader, &mut buf, pos, size, &mut t, last, &mut |c| {
        whole.update(c);
        Ok(())
    })?;
    let actual: [u8; 32] = whole.finalize().into();
    if actual != manifest.setup_sha256 {
        return Err(setup_mismatch(hex(&manifest.setup_sha256), hex(&actual)));
    }
    Ok(())
}

/// Streams `[from, to)` of the reader through `sink` in chunks.
fn pump(
    reader: &mut SetupReader,
    buf: &mut [u8],
    from: u64,
    to: u64,
    t: &mut Tracker,
    current: &'static str,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), ImportError>,
) -> Result<(), ImportError> {
    let mut pos = from;
    while pos < to {
        let n = (to - pos).min(buf.len() as u64) as usize;
        reader.read_at(pos, &mut buf[..n])?;
        sink(&buf[..n])?;
        pos += n as u64;
        t.advance(n as u64, current)?;
    }
    Ok(())
}

/// Copies each manifest file from an installed game folder, looking names up
/// ASCII case-insensitively.
fn copy_installed(
    dir: &Path,
    game: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<(), ImportError> {
    let root = install_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let mut t = Tracker {
        done: 0,
        total: manifest.total_size(),
        progress,
        cancel,
    };
    let mut buf = vec![0u8; CHUNK];
    for entry in manifest.files {
        t.check_cancel()?;
        let src = find_path(&root, entry.path)
            .filter(|p| p.is_file())
            .ok_or_else(|| ImportError::Missing(entry.path.into()))?;
        let mut file = File::open(&src).map_err(ImportError::io(&src))?;
        let len = file.metadata().map_err(ImportError::io(&src))?.len();
        if len != entry.size {
            return Err(ImportError::Mismatch {
                file: entry.path.into(),
                expected: format!("{} bytes", entry.size),
                actual: format!("{len} bytes"),
            });
        }
        let (out_path, mut out) = create_out(game, entry)?;
        let mut h = Sha256::new();
        let mut left = entry.size;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n])
                .map_err(ImportError::io(&src))?;
            h.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(ImportError::io(&out_path))?;
            left -= n as u64;
            t.advance(n as u64, entry.path)?;
        }
        out.sync_all().map_err(ImportError::io(&out_path))?;
        verify(entry, h.finalize().into())?;
    }
    Ok(())
}

fn create_out(game: &Path, entry: &ManifestEntry) -> Result<(PathBuf, File), ImportError> {
    let mut path = game.to_path_buf();
    for c in entry.path.split('/').filter(|c| !c.is_empty()) {
        if c == "." || c == ".." || c.contains('\\') || c.contains(':') {
            return Err(ImportError::Io {
                path: PathBuf::from(entry.path),
                source: io::Error::other("invalid manifest path"),
            });
        }
        path.push(c);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ImportError::io(parent))?;
    }
    let file = File::create(&path).map_err(ImportError::io(&path))?;
    Ok((path, file))
}

fn verify(entry: &ManifestEntry, actual: [u8; 32]) -> Result<(), ImportError> {
    if actual == entry.sha256 {
        Ok(())
    } else {
        Err(ImportError::Mismatch {
            file: entry.path.into(),
            expected: hex(&entry.sha256),
            actual: hex(&actual),
        })
    }
}

fn write_import_json(game: &Path, src: &Source, manifest: &Manifest) -> Result<(), ImportError> {
    let files: Vec<_> = manifest
        .files
        .iter()
        .map(|f| serde_json::json!({ "path": f.path, "sha256": hex(&f.sha256) }))
        .collect();
    let doc = serde_json::json!({
        "format": 1,
        "source_kind": src.kind(),
        "setup_sha256": hex(&manifest.setup_sha256),
        "files": files,
    });
    let path = game.join("import.json");
    let mut bytes = serde_json::to_vec_pretty(&doc).map_err(|e| ImportError::Io {
        path: path.clone(),
        source: io::Error::other(e),
    })?;
    bytes.push(b'\n');
    let mut f = File::create(&path).map_err(ImportError::io(&path))?;
    f.write_all(&bytes)
        .and_then(|_| f.sync_all())
        .map_err(ImportError::io(&path))
}
