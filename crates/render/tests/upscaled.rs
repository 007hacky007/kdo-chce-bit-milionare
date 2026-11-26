//! Upscaled frames from a synthetic pack: hits, every kind of fallback and the LRU.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use render::upscaled::{BlitMode, ContentHash, FrameKey, Miss, PackExpectation, Request};
use render::{
    AssetId, Compositor, DrawOp, Font, Frame, FrameIdent, FrameSource, FrameView, UpscaleStats,
    UpscaledPack,
};
use upscale::{Codec, PackError, PackHeader, PackWriter, RgbaFrame, RunnerParams, content_hash};

const MANIFEST: [u8; 32] = [7; 32];

fn header(pipeline_version: u32, scale: u32) -> PackHeader {
    PackHeader {
        params: RunnerParams {
            runner_sha256: [1; 32],
            model_param_sha256: [2; 32],
            model_bin_sha256: [3; 32],
            model_name: "test".into(),
            scale,
            tile: 128,
            tta: false,
            gpu: 0,
        },
        source_manifest_sha256: MANIFEST,
        pipeline_version,
    }
}

fn palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    for (i, c) in p.iter_mut().enumerate() {
        *c = [i as u8, 255 - i as u8, (i * 7) as u8];
    }
    p
}

/// A source frame with its identity.
struct Src {
    file: String,
    w: u32,
    h: u32,
    px: Vec<u8>,
    pal: Box<[[u8; 3]; 256]>,
    hash: ContentHash,
}

impl Src {
    fn new(file: &str, w: u32, h: u32, px: Vec<u8>) -> Src {
        let pal = Box::new(palette());
        let hash = content_hash(&px, &pal, w, h);
        Src {
            file: file.into(),
            w,
            h,
            px,
            pal,
            hash,
        }
    }
}

#[derive(Default)]
struct Store(HashMap<u32, Src>);

impl FrameSource for Store {
    fn smk_frame(&self, asset: AssetId, frame: u32) -> Option<FrameView<'_>> {
        let s = self.0.get(&asset.0).filter(|_| frame == 1)?;
        Some(FrameView {
            width: s.w,
            height: s.h,
            pixels: &s.px,
            palette: &s.pal,
            ident: Some(FrameIdent {
                file: &s.file,
                frame,
                hash: s.hash,
                stream: false,
            }),
        })
    }
}

/// The "upscaled" image of a source frame: a colour no palette entry has, so a hit and
/// a fallback are told apart; alpha follows index 0 for transparent files.
fn upscaled(s: &Src, mode: BlitMode, seed: u8) -> RgbaFrame {
    let (w, h) = (s.w * 3, s.h * 3);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let idx = s.px[((y / 3) * s.w + x / 3) as usize];
            let a = if mode == BlitMode::Transparent && idx == 0 {
                0
            } else {
                255
            };
            rgba.extend_from_slice(&[seed, x as u8, y as u8, a]);
        }
    }
    RgbaFrame {
        width: w,
        height: h,
        rgba,
    }
}

fn op(id: u32, x: i32, y: i32, transparent: bool) -> DrawOp {
    DrawOp::Smk {
        asset: AssetId(id),
        frame: 1,
        x,
        y,
        transparent,
    }
}

fn write_pack(dir: &Path, h: &PackHeader, items: &[(&Src, BlitMode, RgbaFrame)]) {
    let mut w = PackWriter::create(&dir.join("frames.upk"), h, Codec::Raw).unwrap();
    for (s, mode, f) in items {
        w.add_frame(&FrameKey::new(&s.file, 1), s.hash, *mode, f)
            .unwrap();
    }
    w.finish().unwrap();
}

fn open(dir: &Path) -> Arc<UpscaledPack> {
    Arc::new(UpscaledPack::open(dir, &MANIFEST, &PackExpectation::current()).unwrap())
}

fn compositor(pack: &Arc<UpscaledPack>, log: &Arc<Mutex<Vec<String>>>) -> Compositor {
    let mut c = Compositor::new(3, Font::bundled());
    c.set_upscaled(Some(pack.clone()));
    let l = log.clone();
    c.set_miss_log(Box::new(move |m| l.lock().unwrap().push(m.to_string())));
    c
}

fn pal_px(i: u8) -> [u8; 4] {
    let c = palette()[i as usize];
    [c[0], c[1], c[2], 255]
}

/// A background (opaque), a sprite with index 0 (transparent) and a file the pack does
/// not have: hits draw the pack's RGBA at 3x positions with binary alpha, the miss draws
/// the palette frame by nearest neighbour and is counted and logged once.
#[test]
fn upscaled_hit_and_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let bg = Src::new("quesansw/qs_bg.smk", 8, 6, vec![9; 48]);
    let spr = Src::new("menus/spr.smk", 2, 2, vec![0, 5, 6, 0]);
    let none = Src::new("menus/none.smk", 1, 1, vec![4]);
    let bg_up = upscaled(&bg, BlitMode::Opaque, 200);
    let spr_up = upscaled(&spr, BlitMode::Transparent, 100);
    write_pack(
        dir.path(),
        &header(upscale::pipeline::PIPELINE_VERSION, 3),
        &[
            (&bg, BlitMode::Opaque, bg_up.clone()),
            (&spr, BlitMode::Transparent, spr_up.clone()),
        ],
    );
    let mut store = Store::default();
    store.0.insert(1, bg);
    store.0.insert(2, spr);
    store.0.insert(3, none);
    let pack = open(dir.path());
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut c = compositor(&pack, &log);
    assert!(c.upscaled_active());
    let frame = Frame {
        ops: vec![op(1, 0, 0, false), op(2, 3, 2, true), op(3, 7, 5, true)],
    };
    let img = c.compose(&frame, &store).clone();
    let up_px = |f: &RgbaFrame, x: u32, y: u32| {
        let i = ((y * f.width + x) * 4) as usize;
        [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2], 255]
    };
    // Background hit everywhere it is not covered.
    assert_eq!(img.pixel(0, 0), up_px(&bg_up, 0, 0));
    assert_eq!(img.pixel(20, 17), up_px(&bg_up, 20, 17));
    // Sprite at logical (3, 2) = canvas (9, 6): its transparent corners show the
    // background, its visible pixels come from the pack, offset by 3x.
    assert_eq!(img.pixel(9, 6), up_px(&bg_up, 9, 6));
    assert_eq!(img.pixel(12, 6), up_px(&spr_up, 3, 0));
    assert_eq!(img.pixel(14, 8), up_px(&spr_up, 5, 2));
    assert_eq!(img.pixel(9, 9), up_px(&spr_up, 0, 3));
    assert_eq!(img.pixel(12, 9), up_px(&bg_up, 12, 9));
    // The miss is the palette colour, nearest neighbour over 3x3.
    for (x, y) in [(21, 15), (23, 17)] {
        assert_eq!(img.pixel(x, y), pal_px(4));
    }
    // Outside the background: black, alpha 255 everywhere.
    assert_eq!(img.pixel(24, 0), [0, 0, 0, 255]);
    assert!(img.as_raw().as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    assert_eq!(c.upscale_stats(), UpscaleStats { hits: 2, misses: 1 });
    c.compose(&frame, &store);
    assert_eq!(c.upscale_stats(), UpscaleStats { hits: 4, misses: 2 });
    let lines = log.lock().unwrap().clone();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("menus/none.smk"), "{lines:?}");
    assert!(lines[0].contains("not in the pack"), "{lines:?}");
    // Each payload was decoded once.
    assert_eq!(pack.decodes(), 2);

    // At another scale the pack is not used: the frame equals the plain compose.
    c.set_scale(2);
    assert!(!c.upscaled_active());
    let plain = Compositor::new(2, Font::bundled())
        .compose(&frame, &store)
        .clone();
    assert_eq!(c.compose(&frame, &store), &plain);
    // Back at 3 without a pack: the original pixels.
    c.set_scale(3);
    c.set_upscaled(None);
    let orig = Compositor::new(3, Font::bundled())
        .compose(&frame, &store)
        .clone();
    assert_eq!(c.compose(&frame, &store), &orig);
    assert_eq!(c.upscale_stats(), UpscaleStats::default());
}

/// Review Focus 1: an entry from another source frame, the other blit mode or the wrong
/// size never shows; the draw falls back to the original pixels.
#[test]
fn wrong_hash_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let a = Src::new("a.smk", 4, 4, (1..=16).collect());
    let b = Src::new("b.smk", 4, 4, (1..=16).map(|i| i * 2).collect());
    let small = Src::new("small.smk", 4, 4, vec![3; 16]);
    // The pack's "a.smk" was made from a different frame (another import or a stale
    // pack): same key, other hash.
    let mut stale = Src::new("a.smk", 4, 4, (1..=16).rev().collect());
    stale.file = a.file.clone();
    let mut wrong_size = upscaled(&small, BlitMode::Transparent, 50);
    wrong_size.width = 6;
    wrong_size.height = 8;
    wrong_size.rgba.truncate(6 * 8 * 4);
    write_pack(
        dir.path(),
        &header(upscale::pipeline::PIPELINE_VERSION, 3),
        &[
            (
                &stale,
                BlitMode::Transparent,
                upscaled(&stale, BlitMode::Transparent, 10),
            ),
            (
                &b,
                BlitMode::Transparent,
                upscaled(&b, BlitMode::Transparent, 20),
            ),
            (&small, BlitMode::Transparent, wrong_size),
        ],
    );
    let pack = open(dir.path());
    fn req(s: &Src, transparent: bool) -> Request<'_> {
        Request {
            file: &s.file,
            frame: 1,
            hash: s.hash,
            src_width: s.w,
            src_height: s.h,
            transparent,
            stream: false,
        }
    }
    assert_eq!(pack.resolve(&req(&a, true)).unwrap_err(), Miss::StaleHash);
    assert!(pack.get(&req(&a, true)).is_none());
    assert!(
        pack.get(&Request {
            hash: stale.hash,
            ..req(&a, true)
        })
        .is_some()
    );
    // `get` applies the blit mode and size checks too.
    assert!(pack.get(&req(&b, false)).is_none());
    assert!(pack.get(&req(&small, true)).is_none());
    // "b.smk" was made for a transparent blit; an opaque draw must not use it.
    assert_eq!(pack.resolve(&req(&b, false)).unwrap_err(), Miss::BlitMode);
    assert!(pack.resolve(&req(&b, true)).is_ok());
    assert_eq!(pack.resolve(&req(&small, true)).unwrap_err(), Miss::Size);

    let mut store = Store::default();
    store.0.insert(1, a);
    store.0.insert(2, b);
    store.0.insert(3, small);
    let frame = Frame {
        ops: vec![op(1, 0, 0, true), op(2, 10, 0, false), op(3, 20, 0, true)],
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut c = compositor(&pack, &log);
    let got = c.compose(&frame, &store).clone();
    let orig = Compositor::new(3, Font::bundled())
        .compose(&frame, &store)
        .clone();
    assert_eq!(got, orig);
    assert_eq!(c.upscale_stats(), UpscaleStats { hits: 0, misses: 3 });
    let lines = log.lock().unwrap().clone();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("a.smk") && l.contains("different frame"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("b.smk") && l.contains("blit mode"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("small.smk") && l.contains("wrong size"))
    );

    // A frame without identity is a miss too.
    struct Anon;
    impl FrameSource for Anon {
        fn smk_frame(&self, _: AssetId, _: u32) -> Option<FrameView<'_>> {
            static PX: [u8; 1] = [7];
            static PAL: [[u8; 3]; 256] = [[1, 2, 3]; 256];
            Some(FrameView {
                width: 1,
                height: 1,
                pixels: &PX,
                palette: &PAL,
                ident: None,
            })
        }
    }
    let mut c = compositor(&pack, &log);
    c.compose(
        &Frame {
            ops: vec![op(1, 0, 0, false)],
        },
        &Anon,
    );
    assert_eq!(c.upscale_stats(), UpscaleStats { hits: 0, misses: 1 });
}

/// Review Focus 3: decoding more than the budget evicts least recently used frames.
#[test]
fn lru_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let srcs: Vec<Src> = (0..10)
        .map(|i| Src::new(&format!("f{i}.smk"), 20, 10, vec![i as u8 + 1; 200]))
        .collect();
    let items: Vec<_> = srcs
        .iter()
        .map(|s| (s, BlitMode::Opaque, upscaled(s, BlitMode::Opaque, 1)))
        .collect();
    write_pack(
        dir.path(),
        &header(upscale::pipeline::PIPELINE_VERSION, 3),
        &items,
    );
    let pack = open(dir.path());
    assert_eq!(pack.budget(), render::upscaled::DEFAULT_BUDGET);
    let frame_bytes = 60 * 30 * 4;
    pack.set_budget(frame_bytes * 3 + frame_bytes / 2);
    let get = |i: usize| {
        pack.get(&Request {
            file: &srcs[i].file,
            frame: 1,
            hash: srcs[i].hash,
            src_width: srcs[i].w,
            src_height: srcs[i].h,
            transparent: false,
            stream: false,
        })
        .unwrap()
    };
    for i in 0..10 {
        let f = get(i);
        assert_eq!((f.width, f.height, f.opaque), (60, 30, true));
        assert!(pack.memory_bytes() <= pack.budget());
    }
    assert_eq!(pack.decodes(), 10);
    assert_eq!(pack.memory_bytes(), frame_bytes * 3);
    // The three most recent stay; using 7 makes 8 the oldest.
    get(9);
    get(7);
    assert_eq!(pack.decodes(), 10);
    get(6);
    assert_eq!(pack.decodes(), 11);
    get(7);
    get(9);
    assert_eq!(pack.decodes(), 11);
    get(8);
    assert_eq!(pack.decodes(), 12);
    // Shrinking the budget evicts at once; a frame larger than the budget is returned
    // but not kept.
    pack.set_budget(frame_bytes / 2);
    assert_eq!(pack.memory_bytes(), 0);
    get(1);
    get(1);
    assert_eq!(pack.decodes(), 14);
    assert_eq!(pack.memory_bytes(), 0);
    // Prefetch decodes in parallel and fills the LRU like single gets.
    pack.set_budget(frame_bytes * 4);
    let reqs: Vec<Request<'_>> = srcs[..4]
        .iter()
        .map(|s| Request {
            file: &s.file,
            frame: 1,
            hash: s.hash,
            src_width: s.w,
            src_height: s.h,
            transparent: false,
            stream: false,
        })
        .collect();
    pack.prefetch(&reqs);
    assert_eq!(pack.decodes(), 18);
    assert_eq!(pack.memory_bytes(), frame_bytes * 4);
    for r in &reqs {
        pack.resolve(r).unwrap();
    }
    assert_eq!(pack.decodes(), 18);
}

/// The pack must match this build's pipeline version and scale and the import.
#[test]
fn open_rejects_other_pipeline_scale_or_import() {
    let s = Src::new("a.smk", 2, 2, vec![1; 4]);
    let up = upscaled(&s, BlitMode::Opaque, 1);
    let v = upscale::pipeline::PIPELINE_VERSION;
    let cur = PackExpectation::current();
    for (h, ok) in [
        (header(v, 3), true),
        (header(v + 1, 3), false),
        (header(v, 4), false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_pack(dir.path(), &h, &[(&s, BlitMode::Opaque, up.clone())]);
        let r = UpscaledPack::open(dir.path(), &MANIFEST, &cur);
        assert_eq!(r.is_ok(), ok, "{h:?}: {r:?}");
        if h.pipeline_version != v {
            assert!(
                matches!(r, Err(PackError::PipelineMismatch { .. })),
                "{r:?}"
            );
        }
        if ok {
            assert!(matches!(
                UpscaledPack::open(dir.path(), &[0; 32], &cur),
                Err(PackError::ManifestMismatch)
            ));
        }
    }
    let empty = tempfile::tempdir().unwrap();
    assert!(UpscaledPack::open(empty.path(), &MANIFEST, &cur).is_err());
}

/// The game requires the pinned runner and model; test-runner packs are rejected.
#[test]
fn pinned_expectation_rejects_other_runner() {
    let s = Src::new("a.smk", 2, 2, vec![1; 4]);
    let up = upscaled(&s, BlitMode::Opaque, 1);
    let hex = |h: &str| -> [u8; 32] {
        std::array::from_fn(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
    };
    let mut real = header(upscale::pipeline::PIPELINE_VERSION, 3);
    real.params = RunnerParams {
        runner_sha256: hex(upscale::runner::PINNED_RUNNER_SHA256S[1]),
        model_param_sha256: hex(upscale::runner::MODEL_PARAM_SHA256),
        model_bin_sha256: hex(upscale::runner::MODEL_BIN_SHA256),
        model_name: upscale::runner::MODEL_NAME.into(),
        scale: 3,
        tile: upscale::runner::TILE,
        tta: false,
        gpu: upscale::runner::GPU,
    };
    let mut fake = real.clone();
    fake.params = upscale::FakeRunner::fake_params();
    let mut other_tile = real.clone();
    other_tile.params.tile = 64;
    for (h, ok) in [(real, true), (fake, false), (other_tile, false)] {
        let dir = tempfile::tempdir().unwrap();
        write_pack(dir.path(), &h, &[(&s, BlitMode::Opaque, up.clone())]);
        let r = UpscaledPack::open(dir.path(), &MANIFEST, &PackExpectation::pinned());
        assert_eq!(r.is_ok(), ok, "{:?}: {r:?}", h.params);
        assert!(UpscaledPack::open(dir.path(), &MANIFEST, &PackExpectation::current()).is_ok());
    }
}

/// A payload that fails its checksum is a miss, reported once, and not retried.
#[test]
fn corrupt_payload_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let s = Src::new("a.smk", 2, 2, vec![1; 4]);
    write_pack(
        dir.path(),
        &header(upscale::pipeline::PIPELINE_VERSION, 3),
        &[(&s, BlitMode::Opaque, upscaled(&s, BlitMode::Opaque, 1))],
    );
    let path = dir.path().join("frames.upk");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[256] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();
    let pack = open(dir.path());
    let r = Request {
        file: &s.file,
        frame: 1,
        hash: s.hash,
        src_width: 2,
        src_height: 2,
        transparent: false,
        stream: false,
    };
    assert_eq!(pack.resolve(&r).unwrap_err(), Miss::Decode);
    assert_eq!(pack.resolve(&r).unwrap_err(), Miss::Decode);
    assert_eq!(pack.decodes(), 1);
}

/// Stream-slot frames (cutscenes) are kept only while they are drawn, so playing a
/// cutscene does not push the screen backgrounds out of the LRU.
#[test]
fn stream_frames_do_not_evict_backgrounds() {
    let dir = tempfile::tempdir().unwrap();
    let srcs: Vec<Src> = (0..12)
        .map(|i| Src::new(&format!("f{i}.smk"), 20, 10, vec![i as u8 + 1; 200]))
        .collect();
    let items: Vec<_> = srcs
        .iter()
        .map(|s| (s, BlitMode::Opaque, upscaled(s, BlitMode::Opaque, 1)))
        .collect();
    write_pack(
        dir.path(),
        &header(upscale::pipeline::PIPELINE_VERSION, 3),
        &items,
    );
    let pack = open(dir.path());
    let frame_bytes = 60 * 30 * 4;
    pack.set_budget(frame_bytes * 2);
    let req = |i: usize, stream: bool| Request {
        file: &srcs[i].file,
        frame: 1,
        hash: srcs[i].hash,
        src_width: 20,
        src_height: 10,
        transparent: false,
        stream,
    };
    // Two backgrounds fill the budget.
    pack.prefetch(&[req(0, false), req(1, false)]);
    assert_eq!(pack.decodes(), 2);
    assert_eq!(pack.memory_bytes(), frame_bytes * 2);
    // Ten cutscene frames, one per compose: each is decoded once (prefetch, then the
    // draw), held only for its own compose and never enters the LRU.
    for i in 2..12 {
        let reqs = [req(0, false), req(i, true)];
        pack.prefetch(&reqs);
        for r in &reqs {
            pack.resolve(r).unwrap();
        }
        assert_eq!(pack.decodes(), i as u64 + 1);
        assert_eq!(pack.memory_bytes(), frame_bytes * 2);
        assert_eq!(pack.stream_bytes(), frame_bytes);
    }
    // The backgrounds are still decoded.
    pack.resolve(&req(1, false)).unwrap();
    assert_eq!(pack.decodes(), 12);
    // The next compose without a cutscene frame drops the held one.
    pack.prefetch(&[req(0, false)]);
    assert_eq!(pack.stream_bytes(), 0);
    // A stream frame whose payload is already in the LRU is used from there.
    pack.prefetch(&[req(0, true)]);
    assert_eq!(pack.decodes(), 12);
    assert_eq!(pack.stream_bytes(), 0);
}

/// The app sets a budget of 0 while the original art is shown: every decoded frame is
/// freed, stream frames included, and nothing is kept until the budget is restored.
#[test]
fn zero_budget_frees_all_decoded_frames() {
    let dir = tempfile::tempdir().unwrap();
    let srcs: Vec<Src> = (0..2)
        .map(|i| Src::new(&format!("f{i}.smk"), 20, 10, vec![i as u8 + 1; 200]))
        .collect();
    let items: Vec<_> = srcs
        .iter()
        .map(|s| (s, BlitMode::Opaque, upscaled(s, BlitMode::Opaque, 1)))
        .collect();
    write_pack(
        dir.path(),
        &header(upscale::pipeline::PIPELINE_VERSION, 3),
        &items,
    );
    let pack = open(dir.path());
    let req = |i: usize, stream: bool| Request {
        file: &srcs[i].file,
        frame: 1,
        hash: srcs[i].hash,
        src_width: 20,
        src_height: 10,
        transparent: false,
        stream,
    };
    pack.prefetch(&[req(0, false), req(1, true)]);
    assert!(pack.memory_bytes() > 0 && pack.stream_bytes() > 0);
    let budget = pack.budget();
    pack.set_budget(0);
    assert_eq!((pack.memory_bytes(), pack.stream_bytes()), (0, 0));
    pack.resolve(&req(0, false)).unwrap();
    assert_eq!(pack.memory_bytes(), 0);
    pack.set_budget(budget);
    pack.resolve(&req(0, false)).unwrap();
    assert_eq!(pack.memory_bytes(), 60 * 30 * 4);
}
