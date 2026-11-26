//! Append-only log file `<data_dir>/milionar.log`. The release exe on Windows has no
//! console, so errors that only went to stderr would be invisible.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// The log is moved to `milionar.log.old` before a write that would find it larger.
pub const MAX_LOG_BYTES: u64 = 1 << 20;

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("milionar.log")
}

static LOG: OnceLock<PathBuf> = OnceLock::new();

/// Chooses the log file for [`log`]; later calls are ignored.
pub fn init(data_dir: &Path) {
    let _ = LOG.set(log_path(data_dir));
}

/// Logs to stderr and, once [`init`] ran, to the file (best effort).
pub fn log(msg: &str) {
    eprintln!("{msg}");
    if let Some(path) = LOG.get() {
        let _ = append(path, MAX_LOG_BYTES, unix_now(), msg);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Appends one timestamped line per line of `msg`; rotates first when the file already
/// exceeds `max_bytes`.
pub fn append(path: &Path, max_bytes: u64, unix_secs: u64, msg: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let _ = fs::remove_file(&old);
        if fs::rename(path, &old).is_err() {
            fs::write(path, b"")?;
        }
    }
    let stamp = timestamp(unix_secs);
    let mut out = String::new();
    for line in msg.lines() {
        out.push_str(&format!("{stamp} {line}\n"));
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(out.as_bytes())
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
fn timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(timestamp(1_709_164_800 + 86_399), "2024-02-29 23:59:59");
    }

    #[test]
    fn appends_stamped_lines_and_rotates_when_too_big() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("milionar.log");
        append(&path, 100, 0, "first\nsecond").unwrap();
        append(&path, 100, 1, "third").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "1970-01-01 00:00:00 first\n1970-01-01 00:00:00 second\n1970-01-01 00:00:01 third\n"
        );
        // Grow past the limit: the next write starts a fresh file, the old one is kept.
        append(&path, 100, 2, &"x".repeat(200)).unwrap();
        append(&path, 100, 3, "fresh").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "1970-01-01 00:00:03 fresh\n"
        );
        let old = fs::read_to_string(dir.path().join("sub").join("milionar.log.old")).unwrap();
        assert!(old.contains(&"x".repeat(200)));
        assert!(fs::metadata(&path).unwrap().len() <= 100);
    }
}
