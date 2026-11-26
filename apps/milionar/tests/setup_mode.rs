//! Startup without imported data: the window opens in setup mode (never a bare error box,
//! an exit or a panic).

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn spawn(data_dir: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_milionar"))
        .arg("--data-dir")
        .arg(data_dir)
        .env("SDL_VIDEODRIVER", "dummy")
        .env("SDL_AUDIODRIVER", "dummy")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Waits for `line` in the app's log while the process keeps running, then kills it.
fn assert_stays_in_setup(data_dir: &Path, line: &str) {
    let mut child = spawn(data_dir);
    let log = data_dir.join("milionar.log");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = String::new();
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            let _ = child.wait();
            panic!("exited with {status} instead of staying in setup; log: {seen}");
        }
        seen = std::fs::read_to_string(&log).unwrap_or_default();
        if seen.contains(line) {
            // Still running a moment later: the setup screen is up, not an exit path.
            std::thread::sleep(Duration::from_millis(300));
            assert!(child.try_wait().unwrap().is_none(), "{seen}");
            let _ = child.kill();
            let _ = child.wait();
            assert!(!seen.contains("panicked"), "{seen}");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("no {line:?} in the log: {seen}");
}

#[test]
fn missing_data_starts_setup() {
    let dir = tempfile::tempdir().unwrap();
    assert_stays_in_setup(dir.path(), "setup: no imported game data");
}

#[test]
fn nonexistent_data_dir_starts_setup() {
    let dir = tempfile::tempdir().unwrap();
    assert_stays_in_setup(
        &dir.path().join("does-not-exist yet"),
        "setup: no imported game data",
    );
}

#[test]
fn game_dir_without_import_json_starts_setup() {
    // A half-copied game folder (no import.json, the importer's last write) is not data.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("game/Cuts")).unwrap();
    std::fs::write(dir.path().join("game/Cuts/Logos.smk"), b"x").unwrap();
    assert_stays_in_setup(dir.path(), "setup: no imported game data");
}

/// Esc on the setup screen quits cleanly (scripted input needs the test hooks).
#[cfg(feature = "test-hooks")]
#[test]
fn escape_quits_setup() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_milionar"))
        .arg("--data-dir")
        .arg(dir.path())
        .env("SDL_VIDEODRIVER", "dummy")
        .env("SDL_AUDIODRIVER", "dummy")
        .env("MILIONAR_SCRIPT", "300:key:Escape")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("setup: no imported game data"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
