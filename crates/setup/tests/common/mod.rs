//! Fixture and helpers shared by the setup integration test binaries.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use setup::{ImportError, Manifest, ManifestEntry, Source, detect_source, import};
use sha2::{Digest, Sha256};

pub fn sha(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// A synthetic release: a fake `Setup.exe` image and its manifest.
pub struct Fixture {
    pub setup: Vec<u8>,
    pub manifest: Manifest,
    pub files: Vec<(&'static str, Vec<u8>)>,
}

pub fn payload(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// Files: three distinct payloads plus `Menus/B.smk` sharing `Menus/A.smk`'s
/// payload, like the real release does for identical files.
pub fn fixture() -> Fixture {
    let distinct: [(&'static str, Vec<u8>); 3] = [
        ("Milionar.exe", payload(1, 5000)),
        ("Audiolib/audio1.dat", payload(2, 20_000)),
        ("Menus/A.smk", payload(3, 777)),
    ];
    let mut setup = b"MZ fake installer stub".to_vec();
    setup.resize(300, 0x90);
    let mut entries = Vec::new();
    let mut files = Vec::new();
    for (path, data) in distinct {
        setup.extend_from_slice(b"zlb\x1a");
        let off = setup.len() as u64;
        setup.extend_from_slice(&data);
        setup.extend_from_slice(b"padding between files");
        entries.push(ManifestEntry {
            path,
            setup_offset: off,
            size: data.len() as u64,
            sha256: sha(&data),
        });
        files.push((path, data));
    }
    let a = entries[2];
    entries.push(ManifestEntry {
        path: "Menus/B.smk",
        ..a
    });
    files.push(("Menus/B.smk", files[2].1.clone()));
    setup.extend_from_slice(b"trailer");
    let manifest = Manifest {
        setup_size: setup.len() as u64,
        setup_sha256: sha(&setup),
        files: Box::leak(entries.into_boxed_slice()),
    };
    Fixture {
        setup,
        manifest,
        files,
    }
}

pub fn no_progress() -> impl FnMut(&setup::Progress) {
    |_| {}
}

pub fn assert_game_matches(data_dir: &Path, fx: &Fixture) {
    let game = data_dir.join("game");
    for (path, data) in &fx.files {
        let got = fs::read(game.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(&got, data, "{path}");
    }
}

pub fn leftovers(data_dir: &Path) -> Vec<String> {
    fs::read_dir(data_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".staging-") || n.starts_with(".old-"))
        .collect()
}

pub fn write_setup(dir: &Path, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("Setup.exe");
    fs::write(&p, bytes).unwrap();
    p
}

/// Imports `iso` (expected to be cut short) and checks it fails with
/// `Incomplete` naming the file before any progress is reported.
pub fn assert_incomplete_iso(iso: &Path, manifest: &Manifest) {
    let src = detect_source(iso).unwrap();
    assert_eq!(src, Source::Iso(iso.to_path_buf()));
    let data = iso.parent().unwrap().join("data");
    let mut calls = 0;
    let err = import(
        &src,
        &data,
        manifest,
        &mut |_| calls += 1,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    let len = fs::metadata(iso).unwrap().len();
    assert!(
        matches!(&err, ImportError::Incomplete { path, actual, expected }
            if path == iso && *actual == len && *expected > len),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains(&iso.display().to_string()), "{msg}");
    assert!(msg.contains(&format!("{len} of")), "{msg}");
    assert_eq!(calls, 0, "failed before any copying");
    assert!(!data.join("game").exists());
    assert!(leftovers(&data).is_empty());
}
