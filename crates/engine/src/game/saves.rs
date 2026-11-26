//! `.sav` files (F3; E2 0x4058b0, 0x405900): `<saves>/Mill_1.sav` .. `Mill_3.sav`.
//!
//! Layout: int32 LE cursor (index of the next order entry), then N int32 LE record ids
//! (a permutation of 1..N; N = 230 / 300 / 270). The original writes one after every
//! question of that difficulty and deletes it when the cursor reaches N.
//!
//! Differences from the original (F11.4 FIX): a file is
//! written to a temporary name and renamed over the old one, and a file that is not
//! exactly 4 + 4N bytes, whose cursor is outside 0..N or whose ids are not a permutation
//! of 1..N is treated as missing (the order is reshuffled and the file rewritten on the
//! next question).

use std::io;
use std::path::{Path, PathBuf};

/// File name of difficulty `level_index` (0 easy, 1 medium, 2 hard).
pub fn sav_path(dir: &Path, level_index: usize) -> PathBuf {
    dir.join(format!("Mill_{}.sav", level_index + 1))
}

/// Read and validate a `.sav` holding `n` ids; `None` = missing or invalid.
pub fn read_sav(path: &Path, n: usize) -> Option<(i32, Vec<i32>)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() != 4 + 4 * n {
        return None;
    }
    let words: Vec<i32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_le_bytes(*c))
        .collect();
    let cursor = words[0];
    let order = words[1..].to_vec();
    if usize::try_from(cursor).map_or(true, |c| c >= n) {
        return None;
    }
    let mut seen = vec![false; n];
    for &id in &order {
        let i = usize::try_from(id).ok().filter(|&i| (1..=n).contains(&i))?;
        if std::mem::replace(&mut seen[i - 1], true) {
            return None;
        }
    }
    Some((cursor, order))
}

/// Write cursor + order atomically (temp file + rename), creating the directory.
pub fn write_sav(path: &Path, cursor: i32, order: &[i32]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = Vec::with_capacity(4 + 4 * order.len());
    bytes.extend(cursor.to_le_bytes());
    for id in order {
        bytes.extend(id.to_le_bytes());
    }
    let tmp = path.with_extension("sav.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

/// `remove` (a missing file is fine).
pub fn delete_sav(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_sav(n: usize) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = sav_path(dir.path(), 0);
        assert_eq!(p.file_name().unwrap(), "Mill_1.sav");
        assert_eq!(read_sav(&p, n), None, "missing file");
        (dir, p)
    }

    #[test]
    fn roundtrip_and_wrong_length() {
        let (_dir, p) = tmp_sav(3);
        write_sav(&p, 2, &[3, 1, 2]).unwrap();
        assert_eq!(read_sav(&p, 3), Some((2, vec![3, 1, 2])));
        assert_eq!(read_sav(&p, 4), None, "wrong length");
        std::fs::write(&p, [0u8; 10]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "truncated");
        delete_sav(&p).unwrap();
        delete_sav(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn out_of_range_ids_rejected() {
        let (_dir, p) = tmp_sav(3);
        write_sav(&p, 0, &[3, 3, 2]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "duplicate id");
        write_sav(&p, 0, &[0, 1, 2]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "id 0");
        write_sav(&p, 0, &[1, 2, 4]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "id above N");
        write_sav(&p, 0, &[1, 2, -3]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "negative id");
    }

    #[test]
    fn cursor_out_of_range_rejected() {
        let (_dir, p) = tmp_sav(3);
        write_sav(&p, 3, &[3, 1, 2]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "cursor N");
        write_sav(&p, -1, &[3, 1, 2]).unwrap();
        assert_eq!(read_sav(&p, 3), None, "cursor -1");
        write_sav(&p, 2, &[3, 1, 2]).unwrap();
        assert!(read_sav(&p, 3).is_some(), "cursor N-1 is fine");
    }

    #[test]
    fn save_written_atomically() {
        let (_dir, p) = tmp_sav(3);
        write_sav(&p, 1, &[1, 2, 3]).unwrap();
        // A stale temp file from a crashed write must not matter, and is gone after a write.
        let tmp = p.with_extension("sav.tmp");
        std::fs::write(&tmp, b"half").unwrap();
        assert_eq!(read_sav(&p, 3), Some((1, vec![1, 2, 3])), "old file intact");
        write_sav(&p, 2, &[1, 2, 3]).unwrap();
        assert!(!tmp.exists(), "temp renamed over the target");
        assert_eq!(read_sav(&p, 3), Some((2, vec![1, 2, 3])));
        // Unwritable target (a directory in the way): error, no stray temp file left readable as the save.
        let blocked = p.with_file_name("Mill_2.sav");
        std::fs::create_dir(&blocked).unwrap();
        assert!(write_sav(&blocked, 0, &[1, 2, 3]).is_err());
        assert_eq!(read_sav(&blocked, 3), None);
    }
}
