//! Locates the game data directory and looks up assets inside it.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// Default per-user data directory, matching SDL3
/// `SDL_GetPrefPath("milionar-port", "Milionar")`.
pub fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("milionar-port")
        .join("Milionar")
}

/// Index of the files under `<data_dir>/game`.
#[derive(Debug)]
pub struct GameData {
    game_dir: PathBuf,
    /// Lowercase relative path with `/` separators -> real path, `None`
    /// when several files differ only in case.
    by_path: HashMap<String, Option<PathBuf>>,
    /// Lowercase bare file name -> real path, `None` when not unique.
    by_name: HashMap<String, Option<PathBuf>>,
    /// All indexed files, sorted.
    files: Vec<PathBuf>,
}

impl GameData {
    /// Indexes `data_dir/game/**` once. Directory symlinks are not followed.
    pub fn open(data_dir: &Path) -> io::Result<GameData> {
        let game_dir = data_dir.join("game");
        let mut files = Vec::new();
        walk(&game_dir, &mut files).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot index {}: {e}", game_dir.display()),
            )
        })?;
        files.sort();
        let mut by_path: HashMap<String, Option<PathBuf>> = HashMap::new();
        let mut by_name: HashMap<String, Option<PathBuf>> = HashMap::new();
        for f in &files {
            let rel = f.strip_prefix(&game_dir).unwrap_or(f);
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
                .collect();
            let leaf = parts.last().cloned().unwrap_or_default();
            by_path
                .entry(parts.join("/"))
                .and_modify(|e| *e = None)
                .or_insert_with(|| Some(f.clone()));
            by_name
                .entry(leaf)
                .and_modify(|e| *e = None)
                .or_insert_with(|| Some(f.clone()));
        }
        Ok(GameData {
            game_dir,
            by_path,
            by_name,
            files,
        })
    }

    /// The `game` directory that was indexed.
    pub fn game_dir(&self) -> &Path {
        &self.game_dir
    }

    /// All indexed files (full paths), sorted.
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(PathBuf::as_path)
    }

    /// Finds a file by relative path (`\\` or `/`, ASCII case-insensitive;
    /// empty and `.` segments are ignored) or by bare file name when that name
    /// is unique in the tree. Ambiguous names resolve to `None`.
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        let key = normalize(name);
        if let Some(p) = self.by_path.get(&key) {
            return p.clone();
        }
        if key.contains('/') {
            return None;
        }
        self.by_name.get(&key).cloned().flatten()
    }

    /// Reads a file found by [`resolve`](Self::resolve).
    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        match self.resolve(name) {
            Some(p) => std::fs::read(&p)
                .map_err(|e| io::Error::new(e.kind(), format!("cannot read {}: {e}", p.display()))),
            None if self.is_ambiguous(&normalize(name)) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("ambiguous game file {name}"),
            )),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("game file not found: {name}"),
            )),
        }
    }

    fn is_ambiguous(&self, key: &str) -> bool {
        match self.by_path.get(key) {
            Some(p) => p.is_none(),
            None => !key.contains('/') && matches!(self.by_name.get(key), Some(None)),
        }
    }
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
        .to_ascii_lowercase()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            walk(&path, out)?;
        } else if ty.is_file() || (ty.is_symlink() && path.is_file()) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    #[test]
    fn resolve_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/Quesansw/Mm_regis.smk", b"x");
        let gd = GameData::open(dir.path()).unwrap();
        assert!(gd.resolve("Mm_Regis.Smk").is_some());
        assert!(gd.resolve("QUESANSW\\MM_REGIS.SMK").is_some());
        assert!(gd.resolve("quesansw/mm_regis.smk").is_some());
        assert!(gd.resolve("other/mm_regis.smk").is_none());
    }

    #[test]
    fn resolve_ambiguous_bare_name_is_none() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/A/same.smk", b"1");
        touch(dir.path(), "game/B/same.smk", b"2");
        let gd = GameData::open(dir.path()).unwrap();
        assert!(gd.resolve("same.smk").is_none());
        assert!(gd.resolve("A/same.smk").is_some());
    }

    #[test]
    fn read_missing_names_file() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/A/x.dat", b"1");
        let gd = GameData::open(dir.path()).unwrap();
        assert_eq!(gd.read("A/x.dat").unwrap(), b"1");
        let err = gd.read("Nope/missing.dat").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("Nope/missing.dat"));
    }

    #[test]
    fn only_game_subtree_is_indexed() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/Db/a.dat", b"1");
        touch(dir.path(), ".staging-1/game/b.dat", b"2");
        touch(dir.path(), "stray.dat", b"3");
        let gd = GameData::open(dir.path()).unwrap();
        assert!(gd.resolve("b.dat").is_none());
        assert!(gd.resolve("stray.dat").is_none());
        assert_eq!(gd.files().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/Db/a.dat", b"1");
        std::os::unix::fs::symlink(dir.path().join("game"), dir.path().join("game/Db/loop"))
            .unwrap();
        let gd = GameData::open(dir.path()).unwrap();
        assert_eq!(gd.files().count(), 1);
    }

    #[test]
    fn lookup_keys_are_normalized() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/Db/x.dat", b"1");
        let gd = GameData::open(dir.path()).unwrap();
        for n in [
            "./Db/x.dat",
            "Db//x.dat",
            "\\Db\\x.dat",
            "/db/./X.DAT",
            "x.dat",
        ] {
            assert!(gd.resolve(n).is_some(), "{n}");
        }
        assert!(gd.resolve("").is_none());
        assert!(gd.resolve("../x.dat").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn case_colliding_paths_are_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "game/Db/A.dat", b"1");
        touch(dir.path(), "game/Db/a.dat", b"2");
        let gd = GameData::open(dir.path()).unwrap();
        if gd.files().count() == 2 {
            assert!(gd.resolve("Db/a.dat").is_none());
            let err = gd.read("Db/a.dat").unwrap_err();
            assert!(err.to_string().contains("ambiguous game file Db/a.dat"));
        }
    }

    #[test]
    fn missing_game_dir_names_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = GameData::open(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("game"));
    }

    #[test]
    fn default_data_dir_ends_with_app_name() {
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(
            default_data_dir(),
            base.join("milionar-port").join("Milionar")
        );
    }
}
