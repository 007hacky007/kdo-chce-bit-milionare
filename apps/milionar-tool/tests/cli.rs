use std::process::Command;

fn tool() -> Command {
    Command::new(env!("CARGO_BIN_EXE_milionar-tool"))
}

#[test]
fn cli_help_lists_commands() {
    let out = tool().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for name in [
        "import",
        "verify",
        "coverage",
        "export-frames",
        "questions",
        "screenshot",
        "upscale",
    ] {
        assert!(text.contains(name), "missing {name} in:\n{text}");
    }
}

#[test]
fn cli_import_bad_source_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("not-a-game.txt");
    std::fs::write(&src, b"hello").unwrap();
    let out = tool()
        .arg("import")
        .arg(&src)
        .arg("--data-dir")
        .arg(dir.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(&src.display().to_string()), "{err}");
}

#[test]
fn cli_verify_empty_data_dir_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let out = tool()
        .arg("verify")
        .arg("--data-dir")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn cli_coverage_corpus() {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return;
    };
    let out = tool()
        .arg("coverage")
        .arg("--data-dir")
        .arg(&root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("frames 3235"), "{text}");
}

#[test]
fn cli_export_frames_corpus() {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return;
    };
    let out_dir = tempfile::tempdir().unwrap();
    let out = tool()
        .args(["export-frames", "Mm_dbut.smk"])
        .arg(out_dir.path())
        .arg("--data-dir")
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_dir.path().join("0000.png").is_file());
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out_dir.path().join("frames.json")).unwrap())
            .unwrap();
    assert!(meta["width"].as_u64().unwrap() > 0);
    assert!(meta["frame_duration_us"].as_u64().unwrap() > 0);
    assert!(!meta["frames"].as_array().unwrap().is_empty());
}

#[test]
fn cli_coverage_empty_data_dir_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let out = tool()
        .arg("coverage")
        .arg("--data-dir")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
}

#[test]
fn cli_screenshot_bad_script_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.json");
    std::fs::write(&script, br#"{"seed": 1}"#).unwrap();
    let out = tool()
        .args(["screenshot", "--tick", "1", "--script"])
        .arg(&script)
        .arg("--out")
        .arg(dir.path().join("x.png"))
        .arg("--data-dir")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("quit_at"));
}

#[test]
fn cli_screenshot_corpus() {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.json");
    std::fs::write(
        &script,
        br#"{"seed": 3, "quit_at": 1000,
             "rules": [{"screen": "PlayerCount", "delay": 40, "key": "1"}]}"#,
    )
    .unwrap();
    let out = tool()
        .args(["screenshot", "--silent", "--log", "--scale", "2"])
        .args(["--tick", "5,200"])
        .arg("--script")
        .arg(&script)
        .arg("--out")
        .arg(dir.path().join("shot_{tick}.png"))
        .arg("--data-dir")
        .arg(&root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("\tLogos"), "{text}");
    for t in ["000005", "000200"] {
        let f = std::fs::File::open(dir.path().join(format!("shot_{t}.png"))).unwrap();
        let info = png::Decoder::new(std::io::BufReader::new(f))
            .read_info()
            .unwrap()
            .info()
            .clone();
        assert_eq!((info.width, info.height), (1280, 960));
    }
}

/// The test stand-in never runs against the default data dir.
#[test]
fn cli_upscale_fake_runner_needs_data_dir() {
    let home = tempfile::tempdir().unwrap();
    let out = tool()
        .args(["upscale", "--fake-runner"])
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path())
        .env("APPDATA", home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--data-dir"), "{err}");
    let left = walk(home.path());
    assert!(left.is_empty(), "{left:?}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        v.push(e.path());
        if e.file_type().unwrap().is_dir() {
            v.extend(walk(&e.path()));
        }
    }
    v
}

#[test]
fn cli_upscale_with_fake_runner() {
    let dir = tempfile::tempdir().unwrap();
    // No game import: a clear error, exit 2.
    let out = tool()
        .args(["upscale", "--fake-runner", "--data-dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    // An import without Smacker files gives an empty but valid pack.
    std::fs::create_dir(dir.path().join("game")).unwrap();
    std::fs::write(dir.path().join("game/import.json"), b"{}").unwrap();
    let out = tool()
        .args(["upscale", "--fake-runner", "--data-dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("wrote"), "{text}");
    assert!(dir.path().join("upscaled/frames.upk").is_file());
    // The real runner from a missing directory: exit 2 naming the fetch script.
    let out = tool()
        .args(["upscale", "--runner-dir"])
        .arg(dir.path().join("nope"))
        .arg("--data-dir")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("fetch-upscaler.sh") || err.contains("platform"),
        "{err}"
    );
}

/// `--cancel-after` stops the run like an interrupt; the next run resumes from the cache
/// and writes the pack (Review Focus 4, through the CLI).
#[test]
fn cli_upscale_cancel_and_resume_corpus() {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return;
    };
    let src = std::path::Path::new(&root).join("game");
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game");
    std::fs::create_dir_all(game.join("Menus")).unwrap();
    std::fs::copy(src.join("import.json"), game.join("import.json")).unwrap();
    for f in ["Pauseclk.smk", "Mm_paclk.smk", "Mm_dpl.smk"] {
        std::fs::copy(src.join("Menus").join(f), game.join("Menus").join(f)).unwrap();
    }
    let run = |extra: &[&str]| {
        tool()
            .args(["upscale", "--fake-runner", "--batch-size", "2"])
            .args(extra)
            .arg("--data-dir")
            .arg(dir.path())
            .output()
            .unwrap()
    };
    let out = run(&["--cancel-after", "2"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.to_lowercase().contains("cancel"), "{err}");
    assert!(!dir.path().join("upscaled/frames.upk").exists());
    let out = run(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reused: usize = text
        .split(" from cache")
        .next()
        .and_then(|s| s.rsplit(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    assert!(reused >= 2, "{text}");
    assert!(dir.path().join("upscaled/frames.upk").is_file());
}
