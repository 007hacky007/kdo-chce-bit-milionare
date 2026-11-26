//! Alpha handling, the runner subprocess and the pipeline (FakeRunner).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use upscale::alpha::{BlitMode, apply_mask, blit_mode, fill_hidden};
use upscale::pipeline::{self, FrameSource, Options, SourceFrame};
use upscale::runner::Result as UResult;
use upscale::{
    FakeRunner, FrameKey, PackReader, Runner, UpscaleError, Upscaler, content_hash, manifest_hash,
};

fn palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    for (i, c) in p.iter_mut().enumerate() {
        *c = [i as u8, (i * 7) as u8, 255 - i as u8];
    }
    p
}

#[test]
fn alpha_mask_nearest_binary() {
    // 2x2 source: index 0 at top-left and bottom-right.
    let pixels = [0u8, 5, 7, 0];
    let rgb: Vec<u8> = (0..36 * 3).map(|i| i as u8).collect();
    let f = apply_mask(&rgb, &pixels, 2, 2, 3, BlitMode::Transparent).unwrap();
    assert_eq!((f.width, f.height), (6, 6));
    for y in 0..6 {
        for x in 0..6 {
            let p = &f.rgba[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4];
            let hidden = pixels[(y / 3) * 2 + x / 3] == 0;
            assert_eq!(p[3], if hidden { 0 } else { 255 }, "({x},{y})");
            // The runner's RGB is kept, also under transparent pixels.
            let i = (y * 6 + x) * 3;
            assert_eq!(&p[..3], &rgb[i..i + 3]);
        }
    }
    // Opaque files keep every pixel.
    let o = apply_mask(&rgb, &pixels, 2, 2, 3, BlitMode::Opaque).unwrap();
    assert!(o.rgba.chunks(4).all(|p| p[3] == 255));
    // Wrong buffer sizes are refused.
    assert!(apply_mask(&rgb[..rgb.len() - 1], &pixels, 2, 2, 3, BlitMode::Opaque).is_none());
    assert!(apply_mask(&rgb, &pixels[..3], 2, 2, 3, BlitMode::Opaque).is_none());
    assert!(apply_mask(&rgb, &pixels, 2, 2, 0, BlitMode::Opaque).is_none());
}

#[test]
fn hidden_rgb_filled_from_neighbours() {
    let pal = palette();
    let px = |rgb: &[u8], i: usize| [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]];
    // A row: hidden pixels take the nearest visible colour; the tie at
    // index 3 goes to the earlier pixel.
    let row = [0u8, 0, 3, 0, 9];
    let rgb = fill_hidden(&row, &pal, 5, 1, BlitMode::Transparent);
    let want = [pal[3], pal[3], pal[3], pal[3], pal[9]];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(px(&rgb, i), *w, "pixel {i}");
    }
    // 2D: a hidden ring around one visible pixel, and a far corner.
    #[rustfmt::skip]
    let grid = [
        0, 0, 0, 0,
        0, 4, 0, 0,
        0, 0, 0, 0,
        0, 0, 0, 8,
    ];
    let rgb = fill_hidden(&grid, &pal, 4, 4, BlitMode::Transparent);
    assert_eq!(px(&rgb, 0), pal[4]);
    assert_eq!(px(&rgb, 2), pal[4]);
    assert_eq!(px(&rgb, 14), pal[8]);
    assert_eq!(px(&rgb, 11), pal[8]);
    assert_eq!(px(&rgb, 5), pal[4]);
    // Visible pixels are never changed.
    assert_eq!(px(&rgb, 15), pal[8]);
    // Nothing visible: black.
    let rgb = fill_hidden(&[0; 6], &pal, 3, 2, BlitMode::Transparent);
    assert!(rgb.iter().all(|&b| b == 0));
    // Opaque files show index 0 as its palette colour.
    let rgb = fill_hidden(&row, &pal, 5, 1, BlitMode::Opaque);
    assert_eq!(px(&rgb, 0), pal[0]);
    assert_eq!(px(&rgb, 3), pal[0]);
    assert_eq!(px(&rgb, 4), pal[9]);
}

#[test]
fn blit_mode_follows_engine_usage() {
    assert_eq!(blit_mode("cuts/logos.smk"), BlitMode::Opaque);
    assert_eq!(blit_mode("Menus/Opening.smk"), BlitMode::Opaque);
    assert_eq!(blit_mode("quesansw/qs_bg.smk"), BlitMode::Opaque);
    assert_eq!(blit_mode("menus/ans1selc.smk"), BlitMode::Transparent);
    assert_eq!(blit_mode("beat/sp_bg.smk"), BlitMode::Transparent);
}

/// One synthetic frame: width, height, palette indices.
type Frame = (u32, u32, Vec<u8>);

/// In-memory frames by file.
struct Synth {
    files: Vec<(String, Vec<Frame>)>,
    palette: [[u8; 3]; 256],
}

impl FrameSource for Synth {
    fn files(&self) -> Vec<String> {
        self.files.iter().map(|f| f.0.clone()).collect()
    }
    fn frames(
        &self,
        file: &str,
        f: &mut dyn FnMut(&SourceFrame<'_>) -> UResult<()>,
    ) -> UResult<()> {
        let (_, frames) = self.files.iter().find(|x| x.0 == file).unwrap();
        for (i, (w, h, p)) in frames.iter().enumerate() {
            f(&SourceFrame {
                frame: i as u32 + 1,
                width: *w,
                height: *h,
                pixels: p,
                palette: &self.palette,
            })?;
        }
        Ok(())
    }
}

/// 3 files, 30 frames, 22 unique: "b.smk" repeats "a.smk" frames 1-6 and
/// "a.smk" repeats its own frame 1 twice. "cuts/c.smk" is opaque.
fn synth() -> Synth {
    let frame = |seed: u32| {
        let (w, h) = (5 + seed % 3, 4);
        let p: Vec<u8> = (0..w * h)
            .map(|i| {
                if (i + seed).is_multiple_of(4) {
                    0
                } else {
                    (i * 3 + seed) as u8 | 1
                }
            })
            .collect();
        (w, h, p)
    };
    let mut a: Vec<_> = (0..12).map(frame).collect();
    a.push(frame(0));
    a.push(frame(0));
    let b: Vec<_> = (0..6).map(frame).chain((100..104).map(frame)).collect();
    let c: Vec<_> = (200..206).map(frame).collect();
    Synth {
        files: vec![
            ("a.smk".into(), a),
            ("b.smk".into(), b),
            ("cuts/c.smk".into(), c),
        ],
        palette: palette(),
    }
}

fn data_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("game")).unwrap();
    std::fs::write(d.path().join("game/import.json"), b"{\"synthetic\":1}").unwrap();
    d
}

/// The frame a nearest-neighbour runner plus the mask must produce.
fn expected(file: &str, w: u32, h: u32, p: &[u8], pal: &[[u8; 3]; 256]) -> Vec<u8> {
    let mode = blit_mode(file);
    let rgb = fill_hidden(p, pal, w, h, mode);
    let mut out = Vec::new();
    for y in 0..h * 3 {
        for x in 0..w * 3 {
            let i = ((y / 3) * w + x / 3) as usize;
            if mode == BlitMode::Transparent && p[i] == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                out.extend_from_slice(&rgb[i * 3..i * 3 + 3]);
                out.push(255);
            }
        }
    }
    out
}

fn check_pack(d: &Path, s: &Synth) {
    let r = PackReader::open(&upscale::pack_path(d), &manifest_hash(d).unwrap()).unwrap();
    assert_eq!(r.header().params, FakeRunner::fake_params());
    let mut n = 0;
    for (file, frames) in &s.files {
        for (i, (w, h, p)) in frames.iter().enumerate() {
            let key = FrameKey::new(file, i as u32 + 1);
            let hash = content_hash(p, &s.palette, *w, *h);
            let f = r.read_frame(&key, &hash).unwrap().expect("entry present");
            assert_eq!((f.width, f.height), (w * 3, h * 3));
            let want = expected(file, *w, *h, p, &s.palette);
            // JPEG q95 is lossy: alpha exact, RGB close.
            for (a, b) in f.rgba.chunks(4).zip(want.chunks(4)) {
                assert_eq!(a[3], b[3], "{key:?}");
                for c in 0..3 {
                    assert!(a[c].abs_diff(b[c]) <= 24, "{key:?} {a:?} {b:?}");
                }
            }
            n += 1;
        }
    }
    assert_eq!(r.len(), n);
}

#[test]
fn cancel_and_resume() {
    let s = synth();
    let d = data_dir();
    let opts = Options {
        batch_size: 4,
        keep_cache: false,
    };
    // Run 1: cancel once 8 unique frames are done (two batches).
    let fake = FakeRunner::new();
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    let upscaling = |p: &pipeline::Progress| p.phase == pipeline::Phase::Upscale;
    let r = pipeline::run_source(
        &s,
        d.path(),
        &fake,
        &opts,
        &mut |p| {
            if !upscaling(p) {
                return;
            }
            seen.push(p.clone());
            if p.done >= 8 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    );
    assert!(matches!(r, Err(UpscaleError::Cancelled)), "{r:?}");
    let first = fake.images.load(Ordering::Relaxed);
    assert_eq!(first, 8);
    assert_eq!(seen[0].done, 0);
    assert_eq!(seen[0].total, 22);
    assert!(seen[0].eta.is_none());
    assert_eq!(seen.last().unwrap().done, 8);
    assert!(seen.last().unwrap().eta.is_some());
    assert!(!upscale::pack_path(d.path()).exists());
    let cache = upscale::cache_dir(d.path());
    let mut records: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin"))
        .collect();
    records.sort();
    assert_eq!(records.len(), 8);
    // A record that went bad on disk is redone, not used.
    let mut bytes = std::fs::read(&records[0]).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x55;
    std::fs::write(&records[0], bytes).unwrap();

    // Run 2 finishes; every unique frame went through the runner once
    // overall, plus the one damaged record.
    let fake2 = FakeRunner::new();
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    let r = pipeline::run_source(
        &s,
        d.path(),
        &fake2,
        &opts,
        &mut |p| {
            if upscaling(p) {
                seen.push(p.clone())
            }
        },
        &cancel,
    )
    .unwrap();
    assert_eq!(first + fake2.images.load(Ordering::Relaxed), 22 + 1);
    assert_eq!(seen[0].done, 8);
    assert_eq!(seen.last().unwrap().done, 22);
    assert_eq!((r.files, r.frames, r.unique, r.reused), (3, 30, 22, 8));
    assert_eq!(r.upscaled, 15);
    assert!(!r.up_to_date);
    check_pack(d.path(), &s);
    // The cache is gone after the promote; no work files are left.
    assert!(!cache.exists());
    let mut left: Vec<_> = std::fs::read_dir(d.path().join("upscaled"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, [pipeline::LOCK_FILE, "frames.upk"]);

    // Run 3: up to date, the runner is not called.
    let fake3 = FakeRunner::new();
    let r = pipeline::run_source(&s, d.path(), &fake3, &opts, &mut |_| {}, &cancel).unwrap();
    assert!(r.up_to_date);
    assert_eq!(fake3.invocations.load(Ordering::Relaxed), 0);

    // Changed source frames are redone; unchanged ones come from the old
    // pack (seeded) or a kept cache, so run with keep_cache and change one
    // frame.
    let mut s2 = synth();
    s2.files[1].1[9].2[0] = 77;
    let keep = Options {
        batch_size: 4,
        keep_cache: true,
    };
    let fake4 = FakeRunner::new();
    pipeline::run_source(&s2, d.path(), &fake4, &keep, &mut |_| {}, &cancel).unwrap();
    assert_eq!(fake4.images.load(Ordering::Relaxed), 1);
    check_pack(d.path(), &s2);
    assert!(cache.exists());
    let fake5 = FakeRunner::new();
    let mut s3 = synth();
    s3.files[1].1[8].2[0] = 78;
    pipeline::run_source(&s3, d.path(), &fake5, &keep, &mut |_| {}, &cancel).unwrap();
    assert_eq!(fake5.images.load(Ordering::Relaxed), 2);
    check_pack(d.path(), &s3);
}

/// Progress goes Scan, Upscale, Assemble; inside a phase `done` only grows
/// and stays within `total`, every phase reaches its total, and the bar
/// moves during every phase.
#[test]
fn progress_phases_monotonic() {
    use upscale::pipeline::Phase;
    let s = synth();
    let d = data_dir();
    let opts = Options {
        batch_size: 4,
        keep_cache: false,
    };
    let mut seen: Vec<pipeline::Progress> = Vec::new();
    let fake = FakeRunner::new();
    pipeline::run_source(
        &s,
        d.path(),
        &fake,
        &opts,
        &mut |p| seen.push(p.clone()),
        &AtomicBool::new(false),
    )
    .unwrap();
    let phases: Vec<Phase> = seen.iter().map(|p| p.phase).collect();
    assert!(phases.is_sorted(), "{phases:?}");
    for phase in [Phase::Scan, Phase::Upscale, Phase::Assemble] {
        let of: Vec<&pipeline::Progress> = seen.iter().filter(|p| p.phase == phase).collect();
        assert!(of.len() >= 2, "{phase:?}: {of:?}");
        assert_eq!(of[0].done, 0, "{phase:?} starts at 0");
        let total = of[0].total;
        assert!(of.iter().all(|p| p.total == total && p.done <= total));
        assert!(of.windows(2).all(|w| w[0].done <= w[1].done), "{of:?}");
        assert!(of.windows(2).all(|w| w[0].elapsed <= w[1].elapsed));
        assert_eq!(of.last().unwrap().done, total, "{phase:?} completes");
        assert!(of.last().unwrap().eta.is_some());
    }
    let total = |ph| seen.iter().find(|p| p.phase == ph).unwrap().total;
    assert_eq!(total(Phase::Scan), 3, "files");
    assert_eq!(total(Phase::Upscale), 22, "unique frames");
    assert_eq!(total(Phase::Assemble), 30, "pack entries");
    // Nothing reported before the first file is done has an estimate.
    assert!(seen[0].eta.is_none());

    // An up-to-date pack: only the scan is reported.
    let mut again = Vec::new();
    let r = pipeline::run_source(
        &s,
        d.path(),
        &FakeRunner::new(),
        &opts,
        &mut |p| again.push(p.phase),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(r.up_to_date);
    assert!(again.iter().all(|p| *p == Phase::Scan), "{again:?}");
}

/// After a re-import the old pack (another manifest) seeds the cache: no
/// frame goes through the runner again when the source frames are the same,
/// and only changed frames are upscaled when some differ. A pack from other
/// runner params seeds nothing.
#[test]
fn stale_pack_seeds_the_cache() {
    let s = synth();
    let d = data_dir();
    let opts = Options::default();
    let cancel = AtomicBool::new(false);
    pipeline::run_source(
        &s,
        d.path(),
        &FakeRunner::new(),
        &opts,
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    // Re-import: same frames, new manifest.
    std::fs::write(d.path().join("game/import.json"), b"{\"again\":1}").unwrap();
    let fake = FakeRunner::new();
    let mut seen = Vec::new();
    let r = pipeline::run_source(
        &s,
        d.path(),
        &fake,
        &opts,
        &mut |p| seen.push(p.clone()),
        &cancel,
    )
    .unwrap();
    assert_eq!(fake.invocations.load(Ordering::Relaxed), 0);
    assert_eq!((r.seeded, r.upscaled, r.reused), (22, 0, 0));
    assert!(!r.up_to_date);
    check_pack(d.path(), &s);
    let up: Vec<_> = seen
        .iter()
        .filter(|p| p.phase == pipeline::Phase::Upscale)
        .collect();
    assert_eq!(up.last().unwrap().done, 22);
    // A frame changed: only it is upscaled (b.smk still shows the old one).
    let mut s2 = synth();
    s2.files[0].1[3].2[1] = 99;
    std::fs::write(d.path().join("game/import.json"), b"{\"third\":1}").unwrap();
    let fake = FakeRunner::new();
    let r = pipeline::run_source(&s2, d.path(), &fake, &opts, &mut |_| {}, &cancel).unwrap();
    assert_eq!((r.seeded, r.upscaled, r.unique), (22, 1, 23));
    assert_eq!(fake.images.load(Ordering::Relaxed), 1);
    check_pack(d.path(), &s2);
    // Another runner: nothing is seeded from the fake pack.
    struct Other(FakeRunner);
    impl Upscaler for Other {
        fn params(&self) -> upscale::RunnerParams {
            let mut p = FakeRunner::fake_params();
            p.runner_sha256 = [0xee; 32];
            p
        }
        fn upscale_dir(&self, i: &Path, o: &Path, c: &AtomicBool) -> UResult<()> {
            self.0.upscale_dir(i, o, c)
        }
    }
    std::fs::write(d.path().join("game/import.json"), b"{\"fourth\":1}").unwrap();
    let other = Other(FakeRunner::new());
    let r = pipeline::run_source(&s2, d.path(), &other, &opts, &mut |_| {}, &cancel).unwrap();
    assert_eq!((r.seeded, r.upscaled), (0, 23));
}

/// The free space the upscale step asks for: the full amount, less the cache records
/// this runner can reuse, and nothing when the pack is already current.
#[test]
fn free_space_counts_what_is_there() {
    use pipeline::free_space_needed;
    const FULL: u64 = 1 << 30;
    let s = synth();
    let d = data_dir();
    let params = FakeRunner::fake_params();
    assert_eq!(free_space_needed(d.path(), &params, FULL), FULL);
    // A cancelled run leaves cache records: they count.
    let cancel = AtomicBool::new(false);
    let opts = Options {
        batch_size: 4,
        keep_cache: true,
    };
    let _ = pipeline::run_source(
        &s,
        d.path(),
        &FakeRunner::new(),
        &opts,
        &mut |p| {
            if p.phase == pipeline::Phase::Upscale && p.done >= 8 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    );
    let cached: u64 = std::fs::read_dir(upscale::cache_dir(d.path()))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    assert!(cached > 0);
    assert_eq!(free_space_needed(d.path(), &params, FULL), FULL - cached);
    assert_eq!(
        free_space_needed(d.path(), &params, 1),
        0,
        "never below zero"
    );
    // Records of another runner do not count (the pipeline discards them).
    let mut other = params.clone();
    other.runner_sha256 = [0xee; 32];
    assert_eq!(free_space_needed(d.path(), &other, FULL), FULL);
    // A current pack: nothing to do, no space asked.
    pipeline::run_source(
        &s,
        d.path(),
        &FakeRunner::new(),
        &Options::default(),
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(free_space_needed(d.path(), &params, FULL), 0);
    assert_eq!(free_space_needed(d.path(), &other, FULL), FULL);
    // After a re-import the pack is stale: the full amount again.
    std::fs::write(d.path().join("game/import.json"), b"{\"new\":1}").unwrap();
    assert_eq!(free_space_needed(d.path(), &params, FULL), FULL);
}

fn tree(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            v.push(e.path().display().to_string());
            if e.file_type().unwrap().is_dir() {
                v.extend(tree(&e.path()));
            }
        }
    }
    v.sort();
    v
}

/// A second run while the upscale lock is held returns Busy and leaves the
/// cache, work dirs and staging files alone.
#[test]
fn second_run_while_locked_is_busy() {
    let s = synth();
    let d = data_dir();
    let up = d.path().join("upscaled");
    std::fs::create_dir_all(up.join(".work-old")).unwrap();
    std::fs::create_dir_all(up.join("cache")).unwrap();
    std::fs::write(up.join("cache/stamp.txt"), b"other").unwrap();
    std::fs::write(up.join("cache/00.bin"), b"x").unwrap();
    std::fs::write(up.join(".frames.upk.old.tmp"), b"x").unwrap();
    let before = tree(&up);
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(up.join(pipeline::LOCK_FILE))
        .unwrap();
    fs4::FileExt::try_lock(&held).unwrap();
    let fake = FakeRunner::new();
    let cancel = AtomicBool::new(false);
    let opts = Options::default();
    let r = pipeline::run_source(&s, d.path(), &fake, &opts, &mut |_| {}, &cancel);
    match r {
        Err(UpscaleError::Busy(p)) => assert_eq!(p, up.join(pipeline::LOCK_FILE)),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.invocations.load(Ordering::Relaxed), 0);
    let lock = up.join(pipeline::LOCK_FILE).display().to_string();
    let mut after = tree(&up);
    after.retain(|p| p != &lock);
    assert_eq!(after, before);
    drop(held);

    // A run holds the lock until it returns: a nested run from inside the
    // runner is refused.
    struct Nested<'a> {
        inner: FakeRunner,
        src: &'a Synth,
        data: &'a Path,
        seen: std::sync::Mutex<Vec<String>>,
    }
    impl Upscaler for Nested<'_> {
        fn params(&self) -> upscale::RunnerParams {
            FakeRunner::fake_params()
        }
        fn upscale_dir(&self, i: &Path, o: &Path, c: &AtomicBool) -> UResult<()> {
            let r = pipeline::run_source(
                self.src,
                self.data,
                &FakeRunner::new(),
                &Options::default(),
                &mut |_| {},
                c,
            );
            self.seen.lock().unwrap().push(format!("{r:?}"));
            self.inner.upscale_dir(i, o, c)
        }
    }
    let n = Nested {
        inner: FakeRunner::new(),
        src: &s,
        data: d.path(),
        seen: Default::default(),
    };
    pipeline::run_source(&s, d.path(), &n, &opts, &mut |_| {}, &cancel).unwrap();
    let seen = n.seen.into_inner().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|r| r.starts_with("Err(Busy(")), "{seen:?}");
    check_pack(d.path(), &s);
    // The successful run swept the leftovers.
    assert!(!up.join(".work-old").exists());
    assert!(!up.join(".frames.upk.old.tmp").exists());
}

#[test]
fn cache_from_other_runner_discarded() {
    struct Other(FakeRunner);
    impl Upscaler for Other {
        fn params(&self) -> upscale::RunnerParams {
            let mut p = FakeRunner::fake_params();
            p.runner_sha256 = [0xee; 32];
            p
        }
        fn upscale_dir(&self, i: &Path, o: &Path, c: &AtomicBool) -> UResult<()> {
            self.0.upscale_dir(i, o, c)
        }
    }
    let s = synth();
    let d = data_dir();
    let keep = Options {
        batch_size: 64,
        keep_cache: true,
    };
    let cancel = AtomicBool::new(false);
    let a = Other(FakeRunner::new());
    pipeline::run_source(&s, d.path(), &a, &keep, &mut |_| {}, &cancel).unwrap();
    assert_eq!(a.0.images.load(Ordering::Relaxed), 22);
    // Another runner neither reuses the pack nor the cache.
    let b = FakeRunner::new();
    let r = pipeline::run_source(&s, d.path(), &b, &keep, &mut |_| {}, &cancel).unwrap();
    assert!(!r.up_to_date);
    assert_eq!(r.reused, 0);
    assert_eq!(b.images.load(Ordering::Relaxed), 22);
    check_pack(d.path(), &s);
}

#[test]
fn same_content_opaque_and_transparent_left_out() {
    // The same frame in a transparent file and in a cutscene: the opaque
    // version is stored, the transparent key is left out (fallback).
    let mut s = synth();
    let dup = s.files[2].1[0].clone();
    s.files[0].1.push(dup);
    let d = data_dir();
    let cancel = AtomicBool::new(false);
    let fake = FakeRunner::new();
    let r = pipeline::run_source(
        &s,
        d.path(),
        &fake,
        &Options::default(),
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    assert_eq!(r.conflicts, 1);
    let pr = PackReader::open(
        &upscale::pack_path(d.path()),
        &manifest_hash(d.path()).unwrap(),
    )
    .unwrap();
    assert!(pr.entry(&FrameKey::new("a.smk", 15)).is_none());
    assert!(pr.entry(&FrameKey::new("cuts/c.smk", 1)).is_some());
    assert_eq!(pr.len(), r.frames - 1);
}

/// Rewrites the pack at `d` with `edit` applied to its header and to each
/// file's blit mode, keeping every payload.
fn rewrite_pack(
    d: &Path,
    edit_header: impl Fn(&mut upscale::PackHeader),
    edit_mode: impl Fn(&str, BlitMode) -> BlitMode,
) {
    let path = upscale::pack_path(d);
    let r = PackReader::open(&path, &manifest_hash(d).unwrap()).unwrap();
    let mut h = r.header().clone();
    edit_header(&mut h);
    let mut w = upscale::PackWriter::create(&path, &h, upscale::Codec::DEFAULT).unwrap();
    for k in r.keys() {
        let e = r.entry(k).unwrap();
        let mode = edit_mode(&k.file, r.blit_mode(&k.file).unwrap());
        let payload = r.read_payload(e).unwrap();
        w.add_encoded(
            k,
            e.content_hash,
            mode,
            e.width,
            e.height,
            e.codec,
            &payload,
        )
        .unwrap();
    }
    drop(r);
    w.finish().unwrap();
}

/// A pack from another pipeline version, or with a file recorded under the
/// other blit mode, is rebuilt; the pipeline records both.
#[test]
fn stale_pipeline_or_blit_mode_rebuilds() {
    let s = synth();
    let d = data_dir();
    let cancel = AtomicBool::new(false);
    let opts = Options::default();
    let run = |d: &Path| {
        let fake = FakeRunner::new();
        let r = pipeline::run_source(&s, d, &fake, &opts, &mut |_| {}, &cancel).unwrap();
        (r.up_to_date, fake.images.load(Ordering::Relaxed))
    };
    assert_eq!(run(d.path()), (false, 22));
    let r = PackReader::open(
        &upscale::pack_path(d.path()),
        &manifest_hash(d.path()).unwrap(),
    )
    .unwrap();
    assert_eq!(r.header().pipeline_version, pipeline::PIPELINE_VERSION);
    for f in ["a.smk", "b.smk", "cuts/c.smk"] {
        assert_eq!(r.blit_mode(f), Some(blit_mode(f)), "{f}");
    }
    drop(r);
    assert_eq!(run(d.path()), (true, 0));

    rewrite_pack(d.path(), |h| h.pipeline_version += 1, |_, m| m);
    assert_eq!(run(d.path()), (false, 22));
    assert_eq!(run(d.path()), (true, 0));

    rewrite_pack(
        d.path(),
        |_| {},
        |f, m| match (f, m) {
            ("b.smk", BlitMode::Transparent) => BlitMode::Opaque,
            _ => m,
        },
    );
    assert!(!run(d.path()).0);
    check_pack(d.path(), &s);
    assert_eq!(run(d.path()), (true, 0));
}

#[cfg(unix)]
mod scripts {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("runner.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn dirs(d: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let (i, o) = (d.join("in"), d.join("out"));
        std::fs::create_dir_all(&i).unwrap();
        std::fs::write(i.join("ab.png"), b"x").unwrap();
        (i, o)
    }

    fn run(body: &str) -> (UResult<()>, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let exe = script(d.path(), body);
        let (i, o) = dirs(d.path());
        let r = Runner::unverified(&exe, &d.path().join("models"));
        let res = r.upscale_dir(&i, &o, &AtomicBool::new(false));
        (res, d)
    }

    #[test]
    fn runner_failure_reported() {
        let (r, _d) = run("echo boom >&2; exit 3");
        match r {
            Err(UpscaleError::RunnerFailed { code: 3, stderr }) => assert_eq!(stderr, "boom"),
            other => panic!("{other:?}"),
        }
        let (r, _d) =
            run("echo '[0 Apple M1 Pro] ok' >&2; echo 'invalid gpu device' >&2; exit 255");
        let e = r.unwrap_err();
        assert!(matches!(e, UpscaleError::NoVulkan { .. }), "{e:?}");
        assert!(e.to_string().contains("original art"), "{e}");
        assert!(e.to_string().contains("invalid gpu device"), "{e}");
        let (r, _d) = run("echo 'vkCreateInstance failed -9' >&2; exit 1");
        assert!(matches!(r, Err(UpscaleError::NoVulkan { .. })), "{r:?}");
        // Exit 0 without an output file (the runner does this when it
        // cannot decode an image).
        let (r, _d) = run("echo 'decode image ab.png failed' >&2; exit 0");
        match r {
            Err(UpscaleError::MissingOutput { file, stderr }) => {
                assert_eq!(file, "ab.png");
                assert!(stderr.contains("decode image"));
            }
            other => panic!("{other:?}"),
        }
        let (r, _d) = run("kill -SEGV $$");
        assert!(
            matches!(r, Err(UpscaleError::RunnerCrashed { .. })),
            "{r:?}"
        );
        // Success: outputs present.
        let (r, _d) = run("cp \"$2/ab.png\" \"$4/ab.png\"");
        r.unwrap();
    }

    /// The probe upscales one tiny image and reads the device from stderr;
    /// without Vulkan it fails like a real run.
    #[test]
    fn probe_reports_the_device() {
        let d = tempfile::tempdir().unwrap();
        let models = d.path().join("models");
        let exe = script(
            d.path(),
            "echo '[0 llvmpipe (LLVM 15.0.7, 256 bits)]  queueC=0[1]' >&2; cp \"$2/probe.png\" \"$4/probe.png\"",
        );
        let p = Runner::unverified(&exe, &models)
            .probe(&AtomicBool::new(false))
            .unwrap();
        assert!(p.cpu_only, "{p:?}");
        let exe = script(d.path(), "echo 'vkCreateInstance failed -9' >&2; exit 1");
        let r = Runner::unverified(&exe, &models).probe(&AtomicBool::new(false));
        assert!(matches!(r, Err(UpscaleError::NoVulkan { .. })), "{r:?}");
        // The test stand-in has no device to report.
        assert_eq!(
            FakeRunner::new().probe(&AtomicBool::new(false)).unwrap(),
            upscale::GpuProbe::default()
        );
    }

    #[test]
    fn runner_arguments_fixed() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("args.txt");
        let exe = script(
            d.path(),
            &format!(
                "printf '%s\\n' \"$@\" > '{}'; cp \"$2/ab.png\" \"$4/ab.png\"",
                log.display()
            ),
        );
        let (i, o) = dirs(d.path());
        let models = d.path().join("models");
        Runner::unverified(&exe, &models)
            .upscale_dir(&i, &o, &AtomicBool::new(false))
            .unwrap();
        let got = std::fs::read_to_string(&log).unwrap();
        let want = format!(
            "-i\n{}\n-o\n{}\n-n\nrealesr-animevideov3\n-s\n3\n-t\n128\n-g\n0\n-f\npng\n-m\n{}\n",
            i.display(),
            o.display(),
            models.display()
        );
        assert_eq!(got, want);
    }

    #[test]
    fn runner_cancel_kills_the_process() {
        let d = tempfile::tempdir().unwrap();
        let exe = script(d.path(), "sleep 30");
        let (i, o) = dirs(d.path());
        let cancel = AtomicBool::new(false);
        let t = std::time::Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                cancel.store(true, Ordering::Relaxed);
            });
            let r = Runner::unverified(&exe, &d.path().join("models")).upscale_dir(&i, &o, &cancel);
            assert!(matches!(r, Err(UpscaleError::Cancelled)), "{r:?}");
        });
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }

    /// Cancel kills the runner's whole process group, so a helper it
    /// started does not outlive it.
    #[test]
    fn runner_cancel_kills_the_process_group() {
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("helper.pid");
        let exe = script(
            d.path(),
            &format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()),
        );
        let (i, o) = dirs(d.path());
        let cancel = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                let t = std::time::Instant::now();
                while !pidfile.exists() && t.elapsed() < std::time::Duration::from_secs(5) {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                cancel.store(true, Ordering::Relaxed);
            });
            let r = Runner::unverified(&exe, &d.path().join("models")).upscale_dir(&i, &o, &cancel);
            assert!(matches!(r, Err(UpscaleError::Cancelled)), "{r:?}");
        });
        let pid = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        let t = std::time::Instant::now();
        while alive() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!alive(), "helper {pid} survived the cancel");
    }

    #[test]
    fn pipeline_reports_runner_failure() {
        let d = data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let exe = script(tmp.path(), "echo 'invalid gpu device' >&2; exit 255");
        let r = Runner::unverified(&exe, &tmp.path().join("models"));
        let res = pipeline::run_source(
            &synth(),
            d.path(),
            &r,
            &Options::default(),
            &mut |_| {},
            &AtomicBool::new(false),
        );
        assert!(matches!(res, Err(UpscaleError::NoVulkan { .. })), "{res:?}");
        assert!(!upscale::pack_path(d.path()).exists());
    }
}

#[test]
fn runner_locate_verifies_hashes() {
    let d = tempfile::tempdir().unwrap();
    let r = Runner::locate(d.path());
    if upscale::runner::RUNNER_SHA256.is_none() {
        assert!(matches!(r, Err(UpscaleError::UnsupportedPlatform)));
        return;
    }
    assert!(matches!(r, Err(UpscaleError::RunnerMissing(_))), "{r:?}");
    std::fs::write(
        d.path().join(upscale::runner::RUNNER_EXE),
        b"not the runner",
    )
    .unwrap();
    let e = Runner::locate(d.path()).unwrap_err();
    assert!(matches!(e, UpscaleError::HashMismatch { .. }), "{e:?}");
    assert!(e.to_string().contains("fetch-upscaler.sh"));
}

/// The pins in the code match `tools/upscalers/upscaler.lock.json`.
#[test]
fn pins_match_lock_file() {
    let lock = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/upscalers/upscaler.lock.json"),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&lock).unwrap();
    let runners = v["runners"].as_object().unwrap();
    assert_eq!(
        runners["macos"]["runner"]["sha256"],
        "c1c35d92079085de96b9d547fd7e4464bc8a2e9ccf28d7b8c712d72ade91b7cc"
    );
    if let Some(pin) = upscale::runner::RUNNER_SHA256 {
        let os = if cfg!(windows) { "windows" } else { "macos" };
        assert_eq!(runners[os]["runner"]["sha256"], pin);
    }
    // tools/fetch-upscaler.sh carries the same pins.
    let script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fetch-upscaler.sh"),
    )
    .unwrap();
    for os in ["macos", "windows"] {
        let r = &runners[os];
        let mut pins = vec![&r["archive"]["sha256"], &r["runner"]["sha256"]];
        pins.extend(r["models"].as_object().unwrap().values());
        if let Some(x) = r["extra_files"].as_object() {
            pins.extend(x.values());
        }
        for pin in pins {
            assert!(script.contains(pin.as_str().unwrap()), "{os} {pin}");
        }
        let m = &r["models"];
        assert_eq!(
            m[upscale::runner::MODEL_PARAM_FILE],
            upscale::runner::MODEL_PARAM_SHA256
        );
        assert_eq!(
            m[upscale::runner::MODEL_BIN_FILE],
            upscale::runner::MODEL_BIN_SHA256
        );
    }
}

fn corpus() -> Option<std::path::PathBuf> {
    std::env::var_os("MILIONAR_DATA").map(Into::into)
}

/// Full corpus through the pipeline with the fake runner: 3235 frames
/// keyed, the pack validates, and the four identical Ans*Selc files
/// share one set of payloads (dedupe_identical_frames).
#[test]
fn full_pipeline_with_fake_runner() {
    let Some(data) = corpus() else {
        eprintln!("MILIONAR_DATA not set; skipping");
        return;
    };
    let game = gamedata::GameData::open(&data).unwrap();
    // Work in a temp data dir holding a copy of import.json only.
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("game")).unwrap();
    std::fs::copy(
        data.join("game/import.json"),
        d.path().join("game/import.json"),
    )
    .unwrap();
    let src = pipeline::GameSource::new(&game);
    let fake = FakeRunner::new();
    let cancel = AtomicBool::new(false);
    let r = pipeline::run_source(
        &src,
        d.path(),
        &fake,
        &Options::default(),
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    assert_eq!((r.files, r.frames), (80, 3235));
    assert_eq!(r.conflicts, 0);
    // Each unique frame went through the runner exactly once.
    assert_eq!(fake.images.load(Ordering::Relaxed), r.unique);
    assert_eq!(r.upscaled, r.unique);
    let pr = PackReader::open(
        &upscale::pack_path(d.path()),
        &manifest_hash(d.path()).unwrap(),
    )
    .unwrap();
    assert_eq!(pr.len(), 3235);
    // Every key carries the hash of the frame it was made from.
    for file in src.files() {
        src.frames(&file, &mut |f| {
            let key = FrameKey::new(&file, f.frame);
            let e = pr.entry(&key).expect("key present");
            assert_eq!(
                e.content_hash,
                content_hash(f.pixels, f.palette, f.width, f.height)
            );
            assert_eq!((e.width, e.height), (f.width * 3, f.height * 3));
            Ok(())
        })
        .unwrap();
    }
    // dedupe_identical_frames: Ans1..4Selc point at the same payloads.
    for frame in 1..=6 {
        let e: Vec<_> = (1..=4)
            .map(|n| {
                pr.entry(&FrameKey::new(&format!("menus/ans{n}selc.smk"), frame))
                    .unwrap()
            })
            .collect();
        for x in &e[1..] {
            assert_eq!(
                (x.offset, x.length, x.content_hash),
                (e[0].offset, e[0].length, e[0].content_hash)
            );
        }
    }
    // A few frames decode: an opaque cutscene frame has no transparent
    // pixel, a transparent overlay has some.
    let logos = pr
        .entry(&FrameKey::new("cuts/logos.smk", 1))
        .unwrap()
        .clone();
    let f = pr
        .read_frame(&FrameKey::new("cuts/logos.smk", 1), &logos.content_hash)
        .unwrap()
        .unwrap();
    assert!(f.rgba.chunks(4).all(|p| p[3] == 255));
    // Mm_Pa (the "play again" box) opens from nothing; its last frame is
    // a box with transparent surroundings.
    let key = FrameKey::new("menus/mm_pa.smk", 22);
    let pa = pr.entry(&key).unwrap().clone();
    let f = pr.read_frame(&key, &pa.content_hash).unwrap().unwrap();
    assert!(f.rgba.chunks(4).any(|p| p[3] == 0));
    assert!(f.rgba.chunks(4).any(|p| p[3] == 255));
}
