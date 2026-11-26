use std::path::Path;

use sha2::{Digest, Sha256};
use upscale::{
    BlitMode, Codec, FrameKey, PackError, PackHeader, PackReader, PackWriter, RgbaFrame,
    RunnerParams, content_hash,
};

const MANIFEST: [u8; 32] = [7; 32];

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
        source_manifest_sha256: MANIFEST,
        pipeline_version: 1,
    }
}

/// A synthetic upscaled frame: a gradient with a transparent corner whose
/// hidden RGB continues the gradient, as the runner's output does.
fn frame(w: u32, h: u32, seed: u8) -> RgbaFrame {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if x < w / 4 && y < h / 4 { 0 } else { 255 };
            rgba.extend_from_slice(&[
                (x as u8).wrapping_mul(2).wrapping_add(seed),
                (y as u8).wrapping_mul(3),
                seed,
                a,
            ]);
        }
    }
    RgbaFrame {
        width: w,
        height: h,
        rgba,
    }
}

/// Alpha is exact and transparent pixels decode to zero; visible RGB is
/// exact for raw and close for JPEG.
fn assert_close(got: &RgbaFrame, want: &RgbaFrame, codec: Codec) {
    assert_eq!((got.width, got.height), (want.width, want.height));
    let mut max = 0i32;
    assert_eq!(got.rgba.len(), want.rgba.len());
    for (g, w) in got
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(want.rgba.as_chunks::<4>().0)
    {
        assert_eq!(g[3], w[3]);
        if w[3] == 0 {
            assert_eq!(*g, [0, 0, 0, 0]);
        } else {
            for c in 0..3 {
                max = max.max((g[c] as i32 - w[c] as i32).abs());
            }
        }
    }
    let limit = if codec == Codec::Raw { 0 } else { 12 };
    assert!(max <= limit, "max channel error {max}");
}

fn hash(n: u8) -> [u8; 16] {
    [n; 16]
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn write_sample(path: &Path, codec: Codec) -> Vec<(FrameKey, [u8; 16], RgbaFrame)> {
    let items = vec![
        (
            FrameKey::new("Menus/Ans1Selc.smk", 1),
            hash(1),
            frame(48, 12, 1),
        ),
        (
            FrameKey::new("Menus/Ans1Selc.smk", 2),
            hash(2),
            frame(48, 12, 2),
        ),
        (
            FrameKey::new("menus/ans2selc.smk", 1),
            hash(1),
            frame(48, 12, 1),
        ),
        (
            FrameKey::new("Quesansw/Qs_bg.smk", 3),
            hash(3),
            frame(30, 21, 9),
        ),
    ];
    let mut w = PackWriter::create(path, &header(), codec).unwrap();
    for (k, h, f) in &items {
        w.add_frame(k, *h, BlitMode::Transparent, f).unwrap();
    }
    w.finish().unwrap();
    items
}

#[test]
fn key_is_stable() {
    let mut pal = [[0u8; 3]; 256];
    for (i, c) in pal.iter_mut().enumerate() {
        *c = [i as u8, (i * 7) as u8, (i * 13) as u8];
    }
    let pixels: Vec<u8> = (0..64u32 * 48).map(|i| (i % 251) as u8).collect();
    let a = content_hash(&pixels, &pal, 64, 48);
    assert_eq!(a, content_hash(&pixels.clone(), &pal, 64, 48));
    let mut p2 = pixels.clone();
    p2[1000] ^= 1;
    assert_ne!(a, content_hash(&p2, &pal, 64, 48));
    let mut pal2 = pal;
    pal2[200][1] ^= 1;
    assert_ne!(a, content_hash(&pixels, &pal2, 64, 48));
    // Same bytes, different shape.
    assert_ne!(a, content_hash(&pixels, &pal, 48, 64));
    // Pinned value: the layout in docs/upscale-pack.md must not drift.
    let mut h = Sha256::new();
    h.update(b"MILUPK-FRAME\0\0\0\x01");
    h.update(64u32.to_le_bytes());
    h.update(48u32.to_le_bytes());
    for c in &pal {
        h.update(c);
    }
    h.update(&pixels);
    let full: [u8; 32] = h.finalize().into();
    assert_eq!(a[..], full[..16]);

    assert_eq!(
        FrameKey::new("Menus\\Ans1Selc.SMK", 4),
        FrameKey {
            file: "menus/ans1selc.smk".into(),
            frame: 4
        }
    );
}

#[test]
fn pack_roundtrip() {
    for codec in [Codec::Raw, Codec::DEFAULT] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frames.upk");
        let items = write_sample(&path, codec);
        let r = PackReader::open(&path, &MANIFEST).unwrap();
        assert_eq!(r.header(), &header());
        assert_eq!(r.len(), 4);
        for (k, h, f) in &items {
            let e = r.entry(k).unwrap();
            assert_eq!((e.width, e.height, e.codec), (f.width, f.height, codec));
            let got = r.read_frame(k, h).unwrap().unwrap();
            assert_close(&got, f, codec);
            // A stale source frame never gets the stored image.
            assert_eq!(r.read_frame(k, &hash(99)).unwrap(), None);
        }
        // Identical frames in different files share one payload.
        let a = r.entry(&items[0].0).unwrap();
        let b = r.entry(&items[2].0).unwrap();
        assert_eq!((a.offset, a.length), (b.offset, b.length));
        assert_eq!(
            r.read_frame(&FrameKey::new("menus/ans1selc.smk", 9), &hash(1))
                .unwrap(),
            None
        );
        let keys: Vec<FrameKey> = r.keys().cloned().collect();
        assert_eq!(keys.len(), 4);
        assert_eq!(files_in(dir.path()), vec!["frames.upk".to_string()]);
    }
}

#[test]
fn writer_rejects_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frames.upk");
    let mut w = PackWriter::create(&path, &header(), Codec::Raw).unwrap();
    let f = frame(6, 3, 0);
    w.add_frame(
        &FrameKey::new("a.smk", 1),
        hash(1),
        BlitMode::Transparent,
        &f,
    )
    .unwrap();
    // Duplicate key.
    assert!(
        w.add_frame(
            &FrameKey::new("A.smk", 1),
            hash(2),
            BlitMode::Transparent,
            &f
        )
        .is_err()
    );
    // Frame 0 (keys are 1-based).
    assert!(
        w.add_frame(
            &FrameKey::new("a.smk", 0),
            hash(2),
            BlitMode::Transparent,
            &f
        )
        .is_err()
    );
    // Bad file names.
    for name in ["", "/abs.smk", "a/../b.smk", "a//b.smk"] {
        let k = FrameKey {
            file: name.into(),
            frame: 1,
        };
        assert!(
            w.add_frame(&k, hash(2), BlitMode::Transparent, &f).is_err(),
            "{name:?}"
        );
    }
    // Buffer size does not match the dimensions.
    let bad = RgbaFrame {
        width: 7,
        height: 3,
        rgba: f.rgba.clone(),
    };
    assert!(
        w.add_frame(
            &FrameKey::new("a.smk", 2),
            hash(3),
            BlitMode::Transparent,
            &bad
        )
        .is_err()
    );
    // Raw payload of the wrong size.
    assert!(
        w.add_encoded(
            &FrameKey::new("a.smk", 4),
            hash(4),
            BlitMode::Transparent,
            6,
            3,
            Codec::Raw,
            &[0; 8]
        )
        .is_err()
    );
    // Same content hash with other dimensions.
    assert!(
        w.add_frame(
            &FrameKey::new("a.smk", 3),
            hash(1),
            BlitMode::Transparent,
            &frame(3, 6, 0)
        )
        .is_err()
    );
    w.finish().unwrap();
    assert_eq!(PackReader::open(&path, &MANIFEST).unwrap().len(), 1);
}

fn index_bounds(bytes: &[u8]) -> (usize, usize) {
    let off = u64::from_le_bytes(bytes[192..200].try_into().unwrap()) as usize;
    let len = u64::from_le_bytes(bytes[200..208].try_into().unwrap()) as usize;
    (off, len)
}

fn reseal_index(bytes: &mut [u8]) {
    let (off, len) = index_bounds(bytes);
    let d: [u8; 32] = Sha256::digest(&bytes[off..off + len]).into();
    bytes[208..240].copy_from_slice(&d);
}

fn open_bytes(dir: &Path, bytes: &[u8]) -> Result<PackReader, PackError> {
    let p = dir.join("broken.upk");
    std::fs::write(&p, bytes).unwrap();
    PackReader::open(&p, &MANIFEST)
}

#[test]
fn pack_rejects_truncated_and_mismatched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frames.upk");
    let items = write_sample(&path, Codec::DEFAULT);
    let good = std::fs::read(&path).unwrap();

    // Wrong format version.
    let mut b = good.clone();
    b[8..12].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(
        open_bytes(dir.path(), &b),
        Err(PackError::UnsupportedVersion { found: 1 })
    ));

    // Wrong source manifest hash.
    assert!(matches!(
        PackReader::open(&path, &[8; 32]),
        Err(PackError::ManifestMismatch)
    ));

    // Bad magic.
    let mut b = good.clone();
    b[0] = b'X';
    assert!(matches!(
        open_bytes(dir.path(), &b),
        Err(PackError::BadMagic)
    ));

    // Every truncation fails cleanly.
    for n in 0..good.len() {
        assert!(open_bytes(dir.path(), &good[..n]).is_err(), "len {n}");
    }

    // A truncated entry with a valid index checksum: the last entry's
    // payload length runs into the index.
    let (off, len) = index_bounds(&good);
    let mut b = good.clone();
    let last_len_at = off + len - 8 - 4;
    b[last_len_at..last_len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    reseal_index(&mut b);
    assert!(matches!(
        open_bytes(dir.path(), &b),
        Err(PackError::Invalid(_))
    ));

    // An index cut short with a matching header.
    let mut b = good.clone();
    b.truncate(off + len - 10);
    b[200..208].copy_from_slice(&((len - 10) as u64).to_le_bytes());
    reseal_index(&mut b);
    assert!(open_bytes(dir.path(), &b).is_err());

    // Flipping any single byte never panics and never yields a wrong image.
    let r = PackReader::open(&path, &MANIFEST).unwrap();
    let reference: std::collections::HashMap<FrameKey, RgbaFrame> = items
        .iter()
        .map(|(k, h, _)| (k.clone(), r.read_frame(k, h).unwrap().unwrap()))
        .collect();
    drop(r);
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0x5a;
        if let Ok(r) = open_bytes(dir.path(), &b) {
            for (k, h, _) in &items {
                if let Ok(Some(got)) = r.read_frame(k, h) {
                    assert_eq!(got, reference[k], "byte {i}");
                }
            }
        }
    }
}

#[test]
fn pack_atomic_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frames.upk");

    // An abandoned writer leaves nothing behind.
    {
        let mut w = PackWriter::create(&path, &header(), Codec::DEFAULT).unwrap();
        w.add_frame(
            &FrameKey::new("a.smk", 1),
            hash(1),
            BlitMode::Transparent,
            &frame(9, 9, 1),
        )
        .unwrap();
    }
    assert!(files_in(dir.path()).is_empty());

    // The old pack stays readable until the new one is complete.
    let old = frame(9, 9, 1);
    let mut w = PackWriter::create(&path, &header(), Codec::DEFAULT).unwrap();
    w.add_frame(
        &FrameKey::new("a.smk", 1),
        hash(1),
        BlitMode::Transparent,
        &old,
    )
    .unwrap();
    w.finish().unwrap();

    let new = frame(9, 9, 2);
    let mut w = PackWriter::create(&path, &header(), Codec::DEFAULT).unwrap();
    w.add_frame(
        &FrameKey::new("a.smk", 1),
        hash(2),
        BlitMode::Transparent,
        &new,
    )
    .unwrap();
    let r = PackReader::open(&path, &MANIFEST).unwrap();
    let got = r.read_frame(&FrameKey::new("a.smk", 1), &hash(1)).unwrap();
    assert_close(&got.unwrap(), &old, Codec::DEFAULT);
    drop(r);
    w.finish().unwrap();
    let r = PackReader::open(&path, &MANIFEST).unwrap();
    let got = r.read_frame(&FrameKey::new("a.smk", 1), &hash(2)).unwrap();
    assert_close(&got.unwrap(), &new, Codec::DEFAULT);
    assert_eq!(files_in(dir.path()), vec!["frames.upk".to_string()]);
}

/// Odd sizes and flat areas decode close to the input (guards against the
/// zune-jpeg mis-decode of optimized Huffman tables).
#[test]
fn jpeg_rle_small_and_flat_frames() {
    for (w, h) in [
        (1, 1),
        (9, 9),
        (8, 9),
        (16, 17),
        (17, 16),
        (20, 25),
        (156, 144),
    ] {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (cx, cy) = (x.min(8) as u8, y.min(8) as u8);
                let a = if (x + y) % 7 == 3 { 0 } else { 255 };
                rgba.extend_from_slice(&[cx * 2 + 1, cy * 3, 1, a]);
            }
        }
        let f = RgbaFrame {
            width: w,
            height: h,
            rgba,
        };
        let p = upscale::encode(Codec::JpegRle, &f).unwrap();
        let g = upscale::decode(Codec::JpegRle, w, h, &p).unwrap();
        assert_close(&g, &f, Codec::JpegRle);
        // Wrong dimensions and cut payloads are errors.
        assert!(upscale::decode(Codec::JpegRle, w + 1, h, &p).is_err());
        for n in [0, 3, 4, p.len() / 2, p.len() - 1] {
            assert!(upscale::decode(Codec::JpegRle, w, h, &p[..n]).is_err());
        }
        let mut extra = p.clone();
        extra.push(1);
        assert!(upscale::decode(Codec::JpegRle, w, h, &extra).is_err());
        // Empty interior runs (sum unchanged) are rejected.
        let mut zeros = p.clone();
        zeros.extend_from_slice(&[0, 0]);
        assert!(upscale::decode(Codec::JpegRle, w, h, &zeros).is_err());
    }
}

/// The header records the pipeline version, the index each file's blit
/// mode; one content hash drawn both ways gets two payloads.
#[test]
fn pack_records_pipeline_and_blit_modes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frames.upk");
    let mut h = header();
    h.pipeline_version = 7;
    let mut w = PackWriter::create(&path, &h, Codec::Raw).unwrap();
    let f = frame(6, 3, 4);
    w.add_frame(
        &FrameKey::new("cuts/a.smk", 1),
        hash(1),
        BlitMode::Opaque,
        &f,
    )
    .unwrap();
    w.add_frame(
        &FrameKey::new("b.smk", 1),
        hash(1),
        BlitMode::Transparent,
        &f,
    )
    .unwrap();
    w.add_frame(
        &FrameKey::new("b.smk", 2),
        hash(1),
        BlitMode::Transparent,
        &f,
    )
    .unwrap();
    // One file, one mode.
    assert!(
        w.add_frame(&FrameKey::new("b.smk", 3), hash(2), BlitMode::Opaque, &f)
            .is_err()
    );
    w.finish().unwrap();
    let r = PackReader::open(&path, &MANIFEST).unwrap();
    assert_eq!(r.header().pipeline_version, 7);
    assert_eq!(r.blit_mode("cuts/a.smk"), Some(BlitMode::Opaque));
    assert_eq!(r.blit_mode("b.smk"), Some(BlitMode::Transparent));
    assert_eq!(r.blit_mode("c.smk"), None);
    let a = r.entry(&FrameKey::new("cuts/a.smk", 1)).unwrap();
    let b1 = r.entry(&FrameKey::new("b.smk", 1)).unwrap();
    let b2 = r.entry(&FrameKey::new("b.smk", 2)).unwrap();
    assert_ne!(a.offset, b1.offset);
    assert_eq!(b1.offset, b2.offset);

    // An unknown mode byte is an error. The first file record is "b.smk":
    // u16 name length, the name, then the mode.
    let good = std::fs::read(&path).unwrap();
    let (off, _) = index_bounds(&good);
    assert_eq!(&good[off + 2..off + 7], b"b.smk");
    assert_eq!(good[off + 7], BlitMode::Transparent.id());
    let mut b = good.clone();
    b[off + 7] = 2;
    reseal_index(&mut b);
    assert!(matches!(
        open_bytes(dir.path(), &b),
        Err(PackError::Invalid(_))
    ));
    // The pipeline version lives in the former reserved bytes; the rest of
    // them must stay 0.
    assert_eq!(&good[248..252], &7u32.to_le_bytes());
    let mut b = good.clone();
    b[253] = 1;
    assert!(open_bytes(dir.path(), &b).is_err());
}
