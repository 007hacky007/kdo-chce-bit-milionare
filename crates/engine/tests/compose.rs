//! Corpus test: compose a realistic question-screen frame from the real assets.
//! Run with `--release -- --nocapture` to see the timing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use engine::{Assets, DrawOp, FontSpec, Frame, Rect, TextLayout};
use gamedata::GameData;
use render::{
    Compositor, FrameSource, PackExpectation, RgbaImage, UpscaleStats, UpscaledPack, load_font,
};
use upscale::pipeline::{self, GameSource, Options, SourceFrame};

fn text(s: &str, h: i32, w: i32, rect: Rect, layout: TextLayout) -> DrawOp {
    DrawOp::Text {
        text: s.to_string(),
        font: FontSpec {
            height: h,
            width: w,
        },
        color: [255, 255, 255],
        rect,
        layout,
    }
}

fn root() -> Option<String> {
    let r = std::env::var("MILIONAR_DATA").ok();
    if r.is_none() {
        println!("skipped: MILIONAR_DATA not set");
    }
    r
}

/// A realistic question-screen display list (backdrop, info boxes, host, panels, text).
fn question_frame(a: &mut Assets) -> Frame {
    let mut ops = Vec::new();
    let mut overlay = |a: &mut Assets, name: &str, frame: u32, x: i32, y: i32, t: bool| {
        let asset = a.smk(name).unwrap();
        ops.push(DrawOp::Smk {
            asset,
            frame,
            x,
            y,
            transparent: t,
        });
    };
    overlay(a, "Qs_Bg", 37, 0, 0, false);
    for (i, (x, y)) in [(0, 0), (476, 0), (0, 288), (476, 288)].iter().enumerate() {
        overlay(a, &format!("Mm_Ib{}", i + 1), 1, *x, *y, true);
        overlay(a, "Ms_small", 1, x + 14, y + 146, true);
    }
    overlay(a, "Mm_Regis", 13, 178, 65, true);
    overlay(a, "Qs_PnBox", 8, 20, 0, true);
    overlay(a, "Qs_LlBut", 22, 216, 416, true);
    ops.push(text(
        "Který z následujících českých panovníků nechal postavit Karlův most přes řeku Vltavu?",
        30,
        15,
        Rect::new(58, 82, 590, 187),
        TextLayout::QuestionCentered,
    ));
    for (i, y) in [217, 267, 317, 367].iter().enumerate() {
        let label = [
            "A. Karel IV.",
            "B. Václav II.",
            "C. Jiří z Poděbrad",
            "D. Rudolf II.",
        ][i];
        ops.push(text(
            label,
            20,
            10,
            Rect::new(50, *y, 589, y + 34),
            TextLayout::AnswerCentered,
        ));
    }
    for x in [4, 604] {
        ops.push(text(
            "12",
            20,
            15,
            Rect::new(x, 172, x + 31, 195),
            TextLayout::SingleLineCentered,
        ));
    }
    Frame { ops }
}

#[test]
fn compose_question_screen() {
    let Some(root) = root() else { return };
    let mut a = Assets::new(GameData::open(Path::new(&root)).unwrap());
    let frame = question_frame(&mut a);

    let mut c = Compositor::new(3, load_font());
    let img = c.compose(&frame, &a);
    assert_eq!((img.width(), img.height()), (1920, 1440));
    assert!(
        img.as_raw()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[..3] != [0, 0, 0])
    );

    let n = 50;
    let mut worst = 0f64;
    let start = Instant::now();
    for _ in 0..n {
        let t = Instant::now();
        c.compose(&frame, &a);
        worst = worst.max(t.elapsed().as_secs_f64());
    }
    let avg = start.elapsed().as_secs_f64() / f64::from(n);
    println!(
        "compose s=3 ({} ops, font {:?}): avg {:.2} ms, worst {:.2} ms",
        frame.ops.len(),
        c.font().source(),
        avg * 1e3,
        worst * 1e3
    );
}

/// The game import restricted to some frames of some files (a quick pack for tests).
struct Only {
    inner: GameSource,
    want: BTreeMap<String, BTreeSet<u32>>,
}

impl pipeline::FrameSource for Only {
    fn files(&self) -> Vec<String> {
        self.want.keys().cloned().collect()
    }
    fn frames(
        &self,
        file: &str,
        f: &mut dyn FnMut(&SourceFrame<'_>) -> upscale::runner::Result<()>,
    ) -> upscale::runner::Result<()> {
        let want = &self.want[file];
        self.inner.frames(file, &mut |fr| {
            if want.contains(&fr.frame) {
                f(fr)
            } else {
                Ok(())
            }
        })
    }
}

/// PSNR (dB) of two same-size images over RGB.
fn psnr(a: &RgbaImage, b: &RgbaImage) -> f64 {
    let mut se = 0f64;
    let mut n = 0f64;
    for (p, q) in a
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_raw().as_chunks::<4>().0)
    {
        for k in 0..3 {
            let d = f64::from(p[k]) - f64::from(q[k]);
            se += d * d;
            n += 1.0;
        }
    }
    if se == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / (se / n)).log10()
}

/// The question screen drawn from a pack made by the nearest-neighbour FakeRunner: every
/// Smacker op hits, the picture matches the original-mode compose up to JPEG error, and
/// in release both the first compose (decoding every frame) and the following ones fit
/// the 66 ms tick.
#[test]
fn compose_question_screen_upscaled() {
    let Some(root) = root() else { return };
    let gd = GameData::open(Path::new(&root)).unwrap();
    let mut a = Assets::new(GameData::open(Path::new(&root)).unwrap());
    let frame = question_frame(&mut a);
    // The frames the list draws, by pack key.
    let mut want: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for op in &frame.ops {
        if let DrawOp::Smk { asset, frame, .. } = op {
            let key = a.pack_key_of(*asset).unwrap().to_string();
            want.entry(key).or_default().insert(*frame);
        }
    }
    let smk_ops = frame
        .ops
        .iter()
        .filter(|o| matches!(o, DrawOp::Smk { .. }))
        .count() as u64;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("game")).unwrap();
    std::fs::copy(
        Path::new(&root).join("game/import.json"),
        tmp.path().join("game/import.json"),
    )
    .unwrap();
    let src = Only {
        inner: GameSource::new(&gd),
        want,
    };
    let fake = upscale::FakeRunner::new();
    let report = pipeline::run_source(
        &src,
        tmp.path(),
        &fake,
        &Options::default(),
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.conflicts, 0);
    let manifest = upscale::manifest_hash(Path::new(&root)).unwrap();
    let pack = Arc::new(
        UpscaledPack::open(
            &tmp.path().join("upscaled"),
            &manifest,
            &PackExpectation::current(),
        )
        .unwrap(),
    );

    let mut plain = Compositor::new(3, load_font());
    let want_img = plain.compose(&frame, &a).clone();
    let mut c = Compositor::new(3, load_font());
    c.set_upscaled(Some(pack.clone()));
    let t = Instant::now();
    let img = c.compose(&frame, &a).clone();
    let cold = t.elapsed();
    assert_eq!(
        c.upscale_stats(),
        UpscaleStats {
            hits: smk_ops,
            misses: 0
        }
    );
    let db = psnr(&img, &want_img);
    assert!(db > 35.0, "PSNR {db:.1} dB against the original compose");
    let n = 30;
    let mut worst = Duration::ZERO;
    let t0 = Instant::now();
    for _ in 0..n {
        let t = Instant::now();
        c.compose(&frame, &a);
        worst = worst.max(t.elapsed());
    }
    let avg = t0.elapsed() / n;
    println!(
        "upscaled compose s=3 ({} ops, {} frames decoded, {} KB in the LRU): first {:.2?}, \
         then avg {:.2?}, worst {:.2?}; PSNR vs original {db:.1} dB",
        frame.ops.len(),
        pack.decodes(),
        pack.memory_bytes() / 1024,
        cold,
        avg,
        worst
    );
    assert_eq!(c.upscale_stats().misses, 0);
    if !cfg!(debug_assertions) {
        assert!(cold < Duration::from_millis(66), "first compose {cold:?}");
        assert!(worst < Duration::from_millis(33), "compose {worst:?}");
    }
}

/// Timing on the real pack (`<data>/upscaled/frames.upk`, built with the pinned runner):
/// the question screen cold and warm, and a cutscene played frame by frame (one new
/// full-screen decode per frame). Skipped when the pack is absent or stale. Run with
/// `--release -- --nocapture` for meaningful numbers.
#[test]
fn real_pack_timing() {
    let Some(root) = root() else { return };
    let root = Path::new(&root);
    let manifest = upscale::manifest_hash(root).unwrap();
    let pack = match UpscaledPack::open(
        &root.join("upscaled"),
        &manifest,
        &PackExpectation::current(),
    ) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            println!("skipped: no usable pack ({e})");
            return;
        }
    };
    let mut a = Assets::new(GameData::open(root).unwrap());
    let frame = question_frame(&mut a);
    let mut c = Compositor::new(3, load_font());
    c.set_upscaled(Some(pack.clone()));
    let t = Instant::now();
    c.compose(&frame, &a);
    let cold = t.elapsed();
    let t = Instant::now();
    c.compose(&frame, &a);
    let warm = t.elapsed();
    assert_eq!(c.upscale_stats().misses, 0, "{:?}", c.upscale_stats());
    println!(
        "real pack, question screen: first compose {cold:.2?} ({} decodes), then {warm:.2?}",
        pack.decodes()
    );

    // A cutscene: every frame is new.
    let mut stream = a.cutscene("Cuts/Mm_Open.smk").unwrap();
    let (w, h) = (stream.header().width, stream.header().height);
    let id = a.open_stream("Cuts/Mm_Open.smk", w, h);
    let mut times = Vec::new();
    let mut n = 0;
    while let Some(f) = stream.next_frame().unwrap() {
        n += 1;
        let (px, pal) = (f.pixels.to_vec(), *f.palette);
        a.set_stream_frame(id, n, &px, &pal).unwrap();
        let fr = Frame {
            ops: vec![DrawOp::Smk {
                asset: id,
                frame: n,
                x: 0,
                y: 0,
                transparent: false,
            }],
        };
        // Hash as the snapshot does on the game thread; timed separately.
        let th = Instant::now();
        let _ = a.smk_frame(id, n).unwrap().ident;
        let hash = th.elapsed();
        let t = Instant::now();
        c.compose(&fr, &a);
        times.push((t.elapsed(), hash));
        if n == 60 {
            break;
        }
    }
    times.sort();
    let med = times[times.len() / 2];
    let max = times[times.len() - 1];
    println!(
        "real pack, Mm_Open {n} frames: decode+compose median {:.2?}, max {:.2?}; content \
         hash {:.2?}; LRU {} MB of {} MB",
        med.0,
        max.0,
        med.1,
        pack.memory_bytes() >> 20,
        pack.budget() >> 20
    );
    assert_eq!(c.upscale_stats().misses, 0, "{:?}", c.upscale_stats());
    assert!(pack.memory_bytes() <= pack.budget());
}
