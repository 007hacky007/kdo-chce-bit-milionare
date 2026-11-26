use upscale::{
    BlitMode, Codec, EncodedFrame, FrameCache, FrameKey, PackHeader, PackReader, PackWriter,
    RgbaFrame, RunnerParams,
};

fn frame(w: u32, h: u32, seed: u8) -> RgbaFrame {
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = if x < 2 && y < 2 { 0 } else { 255 };
            rgba.extend_from_slice(&[(x as u8) * 4 + seed, (y as u8) * 4, seed, a]);
        }
    }
    RgbaFrame {
        width: w,
        height: h,
        rgba,
    }
}

fn encoded(seed: u8) -> EncodedFrame {
    let f = frame(24, 15, seed);
    EncodedFrame {
        mode: BlitMode::Transparent,
        width: f.width,
        height: f.height,
        codec: Codec::DEFAULT,
        payload: upscale::encode(Codec::DEFAULT, &f).unwrap(),
    }
}

const T: BlitMode = BlitMode::Transparent;

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn cache_put_get_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let c = FrameCache::open(&d.path().join("cache")).unwrap();
    let h = [0xab; 16];
    assert!(!c.contains(&h, T));
    assert_eq!(c.get(&h, T), None);
    let e = encoded(1);
    c.put(&h, &e).unwrap();
    assert!(c.contains(&h, T));
    assert_eq!(c.get(&h, T), Some(e.clone()));
    // Replacing a record is atomic too.
    let e2 = encoded(2);
    c.put(&h, &e2).unwrap();
    assert_eq!(c.get(&h, T), Some(e2));
    assert_eq!(names(c.dir()), vec![format!("{}.bin", "ab".repeat(16))]);
    // Bad metadata is refused.
    let mut bad = e;
    bad.width = 0;
    assert!(c.put(&[1; 16], &bad).is_err());
}

#[test]
fn cache_corrupted_record_ignored() {
    let d = tempfile::tempdir().unwrap();
    let c = FrameCache::open(d.path()).unwrap();
    let h = [7; 16];
    c.put(&h, &encoded(3)).unwrap();
    let path = d.path().join(format!("{}.bin", "07".repeat(16)));
    let good = std::fs::read(&path).unwrap();
    // Flip a payload byte, truncate, or store under another hash's name.
    let mut flipped = good.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    let other = d.path().join(format!("{}.bin", "08".repeat(16)));
    for (p, bytes) in [
        (&path, flipped),
        (&path, good[..good.len() - 5].to_vec()),
        (&path, good[..10].to_vec()),
        (&other, good.clone()),
    ] {
        std::fs::write(p, &bytes).unwrap();
        let hh = if p == &path { h } else { [8; 16] };
        assert_eq!(c.get(&hh, T), None);
        // The invalid record is deleted.
        assert!(!p.exists());
        assert!(!c.contains(&hh, T));
    }
    // Flipping any byte never yields a different frame.
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0x41;
        std::fs::write(&path, &b).unwrap();
        if let Some(e) = c.get(&h, T) {
            assert_eq!(e, encoded(3), "byte {i}");
        }
    }
}

#[test]
fn cache_orphan_temp_swept() {
    let d = tempfile::tempdir().unwrap();
    let c = FrameCache::open(d.path()).unwrap();
    c.put(&[1; 16], &encoded(1)).unwrap();
    std::fs::write(d.path().join(".abc123.tmp"), b"half").unwrap();
    assert_eq!(c.sweep_temp().unwrap(), 1);
    assert_eq!(names(d.path()), vec![format!("{}.bin", "01".repeat(16))]);

    // The pack writer sweeps stale staging files of crashed writers.
    let pack = d.path().join("frames.upk");
    std::fs::write(d.path().join(".frames.upk.XyZ123.tmp"), b"stale").unwrap();
    let w = PackWriter::create(&pack, &header(), Codec::DEFAULT).unwrap();
    w.finish().unwrap();
    assert_eq!(
        names(d.path()),
        vec![format!("{}.bin", "01".repeat(16)), "frames.upk".into()]
    );
}

fn header() -> PackHeader {
    PackHeader {
        params: RunnerParams {
            runner_sha256: [1; 32],
            model_param_sha256: [2; 32],
            model_bin_sha256: [3; 32],
            model_name: "realesr-animevideov3".into(),
            scale: 3,
            tile: 128,
            tta: false,
            gpu: 0,
        },
        source_manifest_sha256: [5; 32],
        pipeline_version: 1,
    }
}

#[test]
fn pack_assembled_from_cache() {
    let d = tempfile::tempdir().unwrap();
    let c = FrameCache::open(&d.path().join("cache")).unwrap();
    let frames = [
        (FrameKey::new("a.smk", 1), [1u8; 16], 1u8),
        (FrameKey::new("a.smk", 2), [2; 16], 2),
        (FrameKey::new("b.smk", 1), [1; 16], 1),
    ];
    for (_, h, seed) in &frames {
        if !c.contains(h, T) {
            c.put(h, &encoded(*seed)).unwrap();
        }
    }
    let path = d.path().join("frames.upk");
    let mut w = PackWriter::create(&path, &header(), Codec::DEFAULT).unwrap();
    for (k, h, _) in &frames {
        w.add_cached(k, *h, &c.get(h, T).unwrap()).unwrap();
    }
    w.finish().unwrap();
    let r = PackReader::open(&path, &[5; 32]).unwrap();
    for (k, h, seed) in &frames {
        let e = encoded(*seed);
        let want = upscale::decode(e.codec, e.width, e.height, &e.payload).unwrap();
        assert_eq!(r.read_frame(k, h).unwrap(), Some(want));
    }
    // The cache may be removed after promotion.
    std::fs::remove_dir_all(c.dir()).unwrap();
    assert_eq!(r.len(), 3);
}

/// A record made for the other blit is a miss (and is redone).
#[test]
fn cache_mode_mismatch_is_miss() {
    let d = tempfile::tempdir().unwrap();
    let c = FrameCache::open(d.path()).unwrap();
    let h = [9; 16];
    c.put(&h, &encoded(5)).unwrap();
    assert!(c.contains(&h, T));
    assert!(!c.contains(&h, BlitMode::Opaque));
    assert_eq!(c.get(&h, BlitMode::Opaque), None);
    assert!(!c.contains(&h, T));
    let mut o = encoded(5);
    o.mode = BlitMode::Opaque;
    c.put(&h, &o).unwrap();
    assert_eq!(c.get(&h, BlitMode::Opaque), Some(o));
}
