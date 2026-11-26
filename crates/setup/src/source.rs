//! Recognizing what kind of game source the user pointed at.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::ImportError;

/// A place the game data can be imported from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A UDF disc image holding `Setup.exe`.
    Iso(PathBuf),
    /// The release `Setup.exe` itself.
    SetupExe(PathBuf),
    /// A folder where the game was installed by the original installer
    /// (holding `Milionar.exe` and `Audiolib/`, directly or under `app/`).
    Installed(PathBuf),
}

impl Source {
    /// The path the user pointed at.
    pub fn path(&self) -> &Path {
        match self {
            Source::Iso(p) | Source::SetupExe(p) | Source::Installed(p) => p,
        }
    }

    /// Stable name used in `import.json`.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Iso(_) => "iso",
            Source::SetupExe(_) => "setup_exe",
            Source::Installed(_) => "installed",
        }
    }
}

/// Classifies `path` as an ISO, a `Setup.exe` or an installed game folder.
///
/// Files are checked for a UDF signature (`BEA01` at 0x8001) or an `.iso`
/// extension first, then for the name `Setup.exe`. Directories are checked
/// for an installed game first, then for a `Setup.exe` inside, so a mounted
/// disc resolves to `SetupExe(<dir>/Setup.exe)`. Names match ASCII
/// case-insensitively.
pub fn detect_source(path: &Path) -> Result<Source, ImportError> {
    let not = || ImportError::NotRecognized(path.to_path_buf());
    let meta = fs::metadata(path).map_err(|_| not())?;
    if meta.is_file() {
        let iso_ext = path
            .extension()
            .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("iso"));
        if iso_ext || has_udf_signature(path) {
            return Ok(Source::Iso(path.to_path_buf()));
        }
        let setup_name = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("setup.exe"));
        if setup_name {
            return Ok(Source::SetupExe(path.to_path_buf()));
        }
        return Err(not());
    }
    if meta.is_dir() {
        if install_root(path).is_some() {
            return Ok(Source::Installed(path.to_path_buf()));
        }
        if let Some(exe) = find_child(path, "Setup.exe").filter(|p| p.is_file()) {
            return Ok(Source::SetupExe(exe));
        }
    }
    Err(not())
}

fn has_udf_signature(path: &Path) -> bool {
    let mut sig = [0u8; 5];
    fs::File::open(path)
        .and_then(|mut f| {
            f.seek(SeekFrom::Start(0x8001))?;
            f.read_exact(&mut sig)
        })
        .is_ok()
        && &sig == b"BEA01"
}

/// The directory holding `Milionar.exe` and `Audiolib/`: `dir` itself or its
/// `app/` child.
pub(crate) fn install_root(dir: &Path) -> Option<PathBuf> {
    let is_root = |d: &Path| {
        find_child(d, "Milionar.exe").is_some_and(|p| p.is_file())
            && find_child(d, "Audiolib").is_some_and(|p| p.is_dir())
    };
    if is_root(dir) {
        return Some(dir.to_path_buf());
    }
    find_child(dir, "app").filter(|a| a.is_dir() && is_root(a))
}

/// Finds `name` in `dir` and returns it with its on-disk spelling,
/// preferring an exact match, else the first entry (in sorted order) equal
/// to it ignoring ASCII case.
pub(crate) fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut hits: Vec<(bool, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.eq_ignore_ascii_case(name).then(|| (n != name, e.path()))
        })
        .collect();
    hits.sort();
    hits.into_iter().next().map(|(_, p)| p)
}

/// Resolves a `/` separated relative path under `root` case-insensitively.
pub(crate) fn find_path(root: &Path, rel: &str) -> Option<PathBuf> {
    rel.split('/')
        .filter(|c| !c.is_empty())
        .try_fold(root.to_path_buf(), |dir, c| find_child(&dir, c))
}
