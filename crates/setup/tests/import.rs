mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{
    assert_game_matches, assert_incomplete_iso, fixture, leftovers, no_progress, write_setup,
};
use setup::{ImportError, PlayingLock, Source, detect_source, import, recover};

#[test]
fn import_from_setup_exe_copies_and_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let setup = write_setup(&tmp.path().join("src"), &fx.setup);
    let data = tmp.path().join("data");
    let src = Source::SetupExe(setup);
    let mut calls = 0;
    let report = import(
        &src,
        &data,
        &fx.manifest,
        &mut |p| {
            calls += 1;
            assert!(p.done_bytes <= p.total_bytes);
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(calls > 0);
    assert_eq!(report.source, src);
    assert_eq!(report.files, 4);
    assert_eq!(report.bytes, fx.manifest.total_size());
    assert_game_matches(&data, &fx);

    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("game/import.json")).unwrap()).unwrap();
    assert_eq!(json["format"], 1);
    assert_eq!(json["source_kind"], "setup_exe");
    let listed: Vec<&str> = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for (path, _) in &fx.files {
        assert!(listed.contains(path), "{path} not in import.json");
    }
    assert!(leftovers(&data).is_empty());
}

#[test]
fn import_from_installed_folder_any_case() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let app = tmp.path().join("Game");
    fs::create_dir_all(app.join("AUDIOLIB")).unwrap();
    fs::create_dir_all(app.join("MENUS")).unwrap();
    let get = |p: &str| fx.files.iter().find(|f| f.0 == p).unwrap().1.clone();
    fs::write(app.join("milionar.EXE"), get("Milionar.exe")).unwrap();
    fs::write(app.join("AUDIOLIB/AUDIO1.DAT"), get("Audiolib/audio1.dat")).unwrap();
    fs::write(app.join("MENUS/a.SMK"), get("Menus/A.smk")).unwrap();
    fs::write(app.join("MENUS/b.smk"), get("Menus/B.smk")).unwrap();
    let src = detect_source(&app).unwrap();
    assert_eq!(src, Source::Installed(app.clone()));
    let data = tmp.path().join("data");
    let report = import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.files, 4);
    assert_game_matches(&data, &fx);
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("game/import.json")).unwrap()).unwrap();
    assert_eq!(json["source_kind"], "installed");
}

#[test]
fn installed_folder_missing_file_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let app = tmp.path().join("Game");
    fs::create_dir_all(app.join("Audiolib")).unwrap();
    fs::write(app.join("Milionar.exe"), &fx.files[0].1).unwrap();
    let data = tmp.path().join("data");
    let err = import(
        &Source::Installed(app),
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::Missing(p) if p == "Audiolib/audio1.dat"),
        "{err:?}"
    );
    assert!(!data.join("game").exists());
    assert!(leftovers(&data).is_empty());
}

#[test]
fn hash_mismatch_keeps_previous_import() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let data = tmp.path().join("data");
    let good = write_setup(&tmp.path().join("good"), &fx.setup);
    import(
        &Source::SetupExe(good),
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();

    let mut bad = fx.setup.clone();
    let target = fx.manifest.files[1];
    assert_eq!(target.path, "Audiolib/audio1.dat");
    bad[(target.setup_offset + 1234) as usize] ^= 0xff;
    let bad = write_setup(&tmp.path().join("bad"), &bad);
    let err = import(
        &Source::SetupExe(bad),
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::Mismatch { file, .. } if file == "Audiolib/audio1.dat"),
        "{err:?}"
    );
    assert_game_matches(&data, &fx);
    assert!(leftovers(&data).is_empty());
}

#[test]
fn setup_exe_corruption_outside_files_is_a_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let mut bad = fx.setup.clone();
    bad[10] ^= 0xff;
    let bad = write_setup(&tmp.path().join("bad"), &bad);
    let data = tmp.path().join("data");
    let err = import(
        &Source::SetupExe(bad),
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::Mismatch { file, .. } if file == "Setup.exe"),
        "{err:?}"
    );
    assert!(!data.join("game").exists());
    assert!(leftovers(&data).is_empty());

    let short = write_setup(&tmp.path().join("short"), &fx.setup[..100]);
    let err = import(
        &Source::SetupExe(short),
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::Mismatch { file, .. } if file == "Setup.exe"),
        "{err:?}"
    );
}

#[test]
fn cancel_keeps_previous_import() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let data = tmp.path().join("data");
    let setup = write_setup(&tmp.path().join("src"), &fx.setup);
    let src = Source::SetupExe(setup);
    import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();

    let cancel = AtomicBool::new(false);
    let first = fx.manifest.files[0];
    let err = import(
        &src,
        &data,
        &fx.manifest,
        &mut |p| {
            if p.current != first.path {
                cancel.store(true, Ordering::SeqCst);
            }
        },
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(err, ImportError::Cancelled), "{err:?}");
    assert!(cancel.load(Ordering::SeqCst));
    assert_game_matches(&data, &fx);
    assert!(leftovers(&data).is_empty());
}

#[test]
fn recovers_from_crash_between_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let data = tmp.path().join("data");
    fs::create_dir_all(data.join(".old-1234/game")).unwrap();
    fs::write(data.join(".old-1234/game/marker.txt"), b"previous").unwrap();
    fs::write(data.join(".old-1234/game/import.json"), b"{}").unwrap();
    fs::create_dir_all(data.join(".staging-99/game")).unwrap();

    let err = import(
        &Source::SetupExe(tmp.path().join("does-not-exist/Setup.exe")),
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(err, ImportError::Io { .. }), "{err:?}");
    assert_eq!(fs::read(data.join("game/marker.txt")).unwrap(), b"previous");
    assert!(leftovers(&data).is_empty(), "{:?}", leftovers(&data));
}

#[test]
fn busy_when_another_import_holds_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let data = tmp.path().join("data");
    let setup = write_setup(&tmp.path().join("src"), &fx.setup);
    let src = Source::SetupExe(setup);
    import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    // Simulate another import mid-flight: its staging dir and lock.
    fs::create_dir_all(data.join(".staging-4242/game")).unwrap();
    fs::write(data.join(".staging-4242/game/partial"), b"x").unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join(".import.lock"))
        .unwrap();
    fs4::FileExt::try_lock(&lock).unwrap();

    let err = import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::Busy(p) if *p == data),
        "{err:?}"
    );
    assert!(
        err.to_string()
            .contains("another import is already running")
    );
    assert_game_matches(&data, &fx);
    assert!(data.join(".staging-4242/game/partial").is_file());

    drop(lock);
    import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(leftovers(&data).is_empty());
}

#[test]
fn import_refused_while_playing() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let data = tmp.path().join("data");
    let setup = write_setup(&tmp.path().join("src"), &fx.setup);
    let src = Source::SetupExe(setup);
    import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    // Two games may share the data dir; an import may not run under either of them.
    let first = PlayingLock::acquire(&data).unwrap();
    let second = PlayingLock::acquire(&data).unwrap();
    let mut calls = 0;
    let err = import(
        &src,
        &data,
        &fx.manifest,
        &mut |_| calls += 1,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::GameRunning(p) if *p == data),
        "{err:?}"
    );
    assert!(err.to_string().contains("quit the game"), "{err}");
    assert_eq!(calls, 0, "nothing is copied while a game runs");
    assert_game_matches(&data, &fx);
    assert!(leftovers(&data).is_empty());
    drop(first);
    assert!(matches!(
        import(
            &src,
            &data,
            &fx.manifest,
            &mut no_progress(),
            &AtomicBool::new(false)
        ),
        Err(ImportError::GameRunning(_))
    ));
    drop(second);
    import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    // The import released its exclusive hold: the game can start again.
    PlayingLock::acquire(&data).unwrap();
}

#[test]
fn game_cannot_start_during_an_import() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    // What an import holds on the playing lock while it runs.
    let held = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join(".playing.lock"))
        .unwrap();
    fs4::FileExt::try_lock(&held).unwrap();
    let err = PlayingLock::acquire(&data).unwrap_err();
    assert!(
        matches!(&err, ImportError::Busy(p) if *p == data),
        "{err:?}"
    );
    drop(held);
    let lock = PlayingLock::acquire(&data).unwrap();
    assert!(data.join(".playing.lock").is_file());
    drop(lock);
    // A data dir that does not exist yet is created (first run before any import).
    let fresh = tmp.path().join("new dir");
    PlayingLock::acquire(&fresh).unwrap();
}

#[test]
fn unicode_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let setup = write_setup(&tmp.path().join("Kdo chce bít milionáře (CZ)"), &fx.setup);
    let data = tmp.path().join("Uživatel/data");
    let src = detect_source(&setup).unwrap();
    assert_eq!(src, Source::SetupExe(setup.clone()));
    let report = import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.source, Source::SetupExe(setup));
    assert_game_matches(&data, &fx);
}

#[test]
fn detect_source_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();

    let iso = t.join("disc.ISO");
    fs::write(&iso, b"").unwrap();
    assert_eq!(detect_source(&iso).unwrap(), Source::Iso(iso));

    let raw = t.join("disc.bin");
    let mut img = vec![0u8; 0x8010];
    img[0x8001..0x8006].copy_from_slice(b"BEA01");
    fs::write(&raw, &img).unwrap();
    assert_eq!(detect_source(&raw).unwrap(), Source::Iso(raw));

    let exe = t.join("exe/SETUP.EXE");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, b"MZ").unwrap();
    assert_eq!(detect_source(&exe).unwrap(), Source::SetupExe(exe));

    let disc = t.join("mounted");
    fs::create_dir_all(&disc).unwrap();
    fs::write(disc.join("setup.exe"), b"MZ").unwrap();
    assert_eq!(
        detect_source(&disc).unwrap(),
        Source::SetupExe(disc.join("setup.exe"))
    );

    let inst = t.join("inst");
    fs::create_dir_all(inst.join("audiolib")).unwrap();
    fs::write(inst.join("MILIONAR.EXE"), b"MZ").unwrap();
    assert_eq!(
        detect_source(&inst).unwrap(),
        Source::Installed(inst.clone())
    );

    let parent = t.join("parent");
    fs::create_dir_all(parent.join("App/AudioLib")).unwrap();
    fs::write(parent.join("App/Milionar.exe"), b"MZ").unwrap();
    assert_eq!(
        detect_source(&parent).unwrap(),
        Source::Installed(parent.clone())
    );

    let other = t.join("notes.txt");
    fs::write(&other, b"hello").unwrap();
    assert!(matches!(
        detect_source(&other),
        Err(ImportError::NotRecognized(p)) if p == other
    ));
    let empty = t.join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert!(matches!(
        detect_source(&empty),
        Err(ImportError::NotRecognized(_))
    ));
    assert!(matches!(
        detect_source(&t.join("missing")),
        Err(ImportError::NotRecognized(_))
    ));
}

/// Imports the real disc when `MILIONAR_ISO` points at it. Run in release
/// mode; hashing 341 MB in a debug build is slow.
#[test]
fn corpus_import_real_iso() {
    let Some(iso) = std::env::var_os("MILIONAR_ISO") else {
        eprintln!("MILIONAR_ISO not set; skipping");
        return;
    };
    let iso = PathBuf::from(iso);
    let src = detect_source(&iso).unwrap();
    assert_eq!(src, Source::Iso(iso));
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let mut last = None;
    let report = import(
        &src,
        &data,
        &setup::TOPCD,
        &mut |p| last = Some(*p),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.files, 88);
    let last = last.unwrap();
    assert_eq!(last.done_bytes, last.total_bytes);
    let gd = gamedata::GameData::open(&data).unwrap();
    assert!(gd.resolve("Milionar.exe").is_some());
    assert!(gd.resolve("Mm_Regis.Smk").is_some());
    assert_eq!(gd.files().count(), 89, "88 files plus import.json");
}

#[test]
fn refuses_to_replace_foreign_game_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture();
    let data = tmp.path().join("data");
    fs::create_dir_all(data.join("game")).unwrap();
    fs::write(data.join("game/notes.txt"), b"mine").unwrap();
    let setup = write_setup(&tmp.path().join("src"), &fx.setup);
    let src = Source::SetupExe(setup);
    let mut calls = 0;
    let err = import(
        &src,
        &data,
        &fx.manifest,
        &mut |_| calls += 1,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ImportError::ForeignGameDir(p) if *p == data.join("game")),
        "{err:?}"
    );
    assert!(
        err.to_string()
            .contains(&data.join("game").display().to_string())
    );
    assert_eq!(calls, 0, "refused before copying");
    assert_eq!(fs::read(data.join("game/notes.txt")).unwrap(), b"mine");
    assert!(leftovers(&data).is_empty());

    // A plain file named `game` is not replaced either.
    fs::remove_dir_all(data.join("game")).unwrap();
    fs::write(data.join("game"), b"file").unwrap();
    let err = import(
        &src,
        &data,
        &fx.manifest,
        &mut no_progress(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(err, ImportError::ForeignGameDir(_)), "{err:?}");

    // An empty `game` directory is fine, and so is an earlier import.
    fs::remove_file(data.join("game")).unwrap();
    fs::create_dir_all(data.join("game")).unwrap();
    for _ in 0..2 {
        import(
            &src,
            &data,
            &fx.manifest,
            &mut no_progress(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_game_matches(&data, &fx);
    }
    assert!(leftovers(&data).is_empty());
}

#[test]
fn public_recover_restores_and_cleans() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    recover(&data).unwrap();
    assert!(!data.exists(), "recover must not create the data dir");

    fs::create_dir_all(data.join(".old-77/game")).unwrap();
    fs::write(data.join(".old-77/game/import.json"), b"{}").unwrap();
    fs::create_dir_all(data.join(".staging-78/game")).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join(".import.lock"))
        .unwrap();
    fs4::FileExt::try_lock(&lock).unwrap();
    assert!(matches!(recover(&data), Err(ImportError::Busy(_))));
    assert!(data.join(".staging-78").is_dir(), "untouched while busy");
    drop(lock);

    recover(&data).unwrap();
    assert!(data.join("game/import.json").is_file());
    assert!(leftovers(&data).is_empty(), "{:?}", leftovers(&data));
}

/// The real disc cut to its first 400 MB (built in a temp dir from
/// `MILIONAR_ISO`) must fail up front, not after copying most of it.
#[test]
fn corpus_truncated_real_iso_fails_early() {
    let Some(iso) = std::env::var_os("MILIONAR_ISO") else {
        eprintln!("MILIONAR_ISO not set; skipping");
        return;
    };
    use std::io::{Read, Write};
    let tmp = tempfile::tempdir().unwrap();
    let cut = tmp.path().join("cut.iso");
    let mut from = fs::File::open(iso).unwrap().take(400_000_000);
    let mut to = fs::File::create(&cut).unwrap();
    std::io::copy(&mut from, &mut to).unwrap();
    to.flush().unwrap();
    drop(to);
    assert_incomplete_iso(&cut, &setup::TOPCD);
}

/// The upscale step's exclusive hold: refused while a game plays, and while held no
/// game can start and no import can run.
#[test]
fn exclusive_playing_lock_for_the_upscale() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data ěšč");
    let game = PlayingLock::acquire(&data).unwrap();
    let err = PlayingLock::acquire_exclusive(&data).unwrap_err();
    assert!(
        matches!(&err, ImportError::GameRunning(p) if *p == data),
        "{err:?}"
    );
    drop(game);
    let held = PlayingLock::acquire_exclusive(&data).unwrap();
    assert!(matches!(
        PlayingLock::acquire(&data),
        Err(ImportError::Busy(_))
    ));
    assert!(matches!(
        PlayingLock::acquire_exclusive(&data),
        Err(ImportError::GameRunning(_))
    ));
    drop(held);
    PlayingLock::acquire(&data).unwrap();
}
