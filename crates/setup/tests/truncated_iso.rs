//! Tests that spawn child processes live in this separate binary.
//!
//! The import lock is a `flock`, which belongs to the open file description.
//! Spawning a child copies the parent's fd table, and the copy of an
//! `O_CLOEXEC` fd stays alive until the child's exec finishes. If the import
//! tests ran in the same process, a lock fd open while `hdiutil` was spawned
//! kept the lock held for a few ms after that test dropped it, and the
//! test's next `import` intermittently got `Busy`.
#![cfg(target_os = "macos")]

mod common;

use std::fs;
use std::sync::atomic::AtomicBool;

use common::{assert_game_matches, assert_incomplete_iso, fixture, no_progress, write_setup};
use setup::{Source, import};

#[test]
fn truncated_iso_fails_early_and_names_file() {
    use std::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let src = tmp.path().join("src");
    write_setup(&src, &fx.setup);
    let out = tmp.path().join("img");
    let status = Command::new("hdiutil")
        .args(["makehybrid", "-udf", "-udf-version", "1.50", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let iso = tmp.path().join("img.iso");

    // The intact image imports fine.
    let ok = tmp.path().join("ok");
    import(
        &Source::Iso(iso.clone()),
        &ok,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_game_matches(&ok, &fx);

    let mut img = formats::udf::UdfImage::open(fs::File::open(&iso).unwrap()).unwrap();
    let end = img.find("Setup.exe").unwrap().unwrap().recorded_end();
    drop(img);
    let cut = tmp.path().join("cut");
    fs::create_dir_all(&cut).unwrap();
    let cut_iso = cut.join("Kdo chce bít (cut).iso");
    let mut bytes = fs::read(&iso).unwrap();
    bytes.truncate((end - 1) as usize);
    fs::write(&cut_iso, &bytes).unwrap();
    assert_incomplete_iso(&cut_iso, &fx.manifest);
}
