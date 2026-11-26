use formats::FormatError;
use formats::udf::UdfImage;
use sha2::{Digest, Sha256};
use std::io::Cursor;

#[test]
fn udf_rejects_non_udf() {
    let r = UdfImage::open(Cursor::new(vec![0u8; 600_000]));
    assert!(matches!(r, Err(FormatError::Invalid { .. })));
}

#[cfg(target_os = "macos")]
#[test]
fn udf_reads_generated_fixture() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("Docs")).unwrap();
    let pattern: Vec<u8> = (0..307_200u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("Setup.exe"), &pattern).unwrap();
    std::fs::write(src.join("Docs").join("Čti mě.txt"), b"ahoj").unwrap();
    let out = dir.path().join("img");
    let status = Command::new("hdiutil")
        .args(["makehybrid", "-udf", "-udf-version", "1.50", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let iso = dir.path().join("img.iso");
    let mut img = UdfImage::open(std::fs::File::open(&iso).unwrap()).unwrap();

    let f = img.find("setup.exe").unwrap().expect("Setup.exe");
    assert_eq!(f.size, 307_200);
    let mut buf = vec![0u8; 307_200];
    img.read_at(&f, 0, &mut buf).unwrap();
    assert_eq!(buf, pattern);
    let mut mid = [0u8; 100];
    img.read_at(&f, 5000, &mut mid).unwrap();
    assert_eq!(&mid[..], &pattern[5000..5100]);
    assert!(img.read_at(&f, 307_150, &mut mid).is_err());

    let t = img.find("docs/ČTI MĚ.txt").unwrap().expect("unicode name");
    let mut b = [0u8; 4];
    img.read_at(&t, 0, &mut b).unwrap();
    assert_eq!(&b, b"ahoj");
    assert!(img.find("missing").unwrap().is_none());
    assert!(img.find("Docs/missing").unwrap().is_none());
    assert!(img.find("Setup.exe/x").unwrap().is_none());
}

#[test]
fn udf_garbage_at_avdp_does_not_panic() {
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..50 {
        let mut img = vec![0u8; 258 * 2048];
        for b in &mut img[256 * 2048..] {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = (state >> 32) as u8;
        }
        let _ = UdfImage::open(Cursor::new(img));
    }
}

fn stamp(img: &mut [u8], sec: usize, id: u16) {
    let o = sec * 2048;
    img[o..o + 2].copy_from_slice(&id.to_le_bytes());
    let sum = img[o..o + 16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |a, (_, v)| a.wrapping_add(*v));
    img[o + 4] = sum;
}

#[test]
fn udf_garbage_with_valid_tags_does_not_panic() {
    let mut state = 0x0bad_cafe_dead_beefu64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u32
    };
    let mut opened = 0;
    for _ in 0..300 {
        let mut img = vec![0u8; 280 * 2048];
        for b in &mut img[256 * 2048..] {
            *b = next() as u8;
        }
        // Random file entries / directory data from sector 261 on.
        for sec in 261..280usize {
            let o = sec * 2048;
            let id = [261u16, 266, 257, 258][next() as usize % 4];
            stamp(&mut img, sec, id);
            if id == 261 {
                let l_ad = (next() % 80) & !7;
                img[o + 27] = if next() % 2 == 0 { 4 } else { 5 };
                img[o + 34..o + 36].copy_from_slice(&((next() % 4) as u16).to_le_bytes());
                img[o + 56..o + 64].copy_from_slice(&u64::from(next() % 100_000).to_le_bytes());
                img[o + 168..o + 172].copy_from_slice(&0u32.to_le_bytes());
                img[o + 172..o + 176].copy_from_slice(&l_ad.to_le_bytes());
            }
        }
        // Valid anchor, partition (start 260), logical volume and file set.
        let w32 =
            |img: &mut Vec<u8>, o: usize, v: u32| img[o..o + 4].copy_from_slice(&v.to_le_bytes());
        w32(&mut img, 256 * 2048 + 16, 3 * 2048);
        w32(&mut img, 256 * 2048 + 20, 257);
        stamp(&mut img, 256, 2);
        let pd = 257 * 2048;
        img[pd + 22..pd + 24].copy_from_slice(&0u16.to_le_bytes());
        w32(&mut img, pd + 188, 260);
        stamp(&mut img, 257, 5);
        let lv = 258 * 2048;
        w32(&mut img, lv + 212, 2048);
        w32(&mut img, lv + 248, 2048);
        w32(&mut img, lv + 252, 0);
        img[lv + 256..lv + 258].copy_from_slice(&0u16.to_le_bytes());
        w32(&mut img, lv + 264, 6);
        w32(&mut img, lv + 268, 1);
        img[lv + 440..lv + 446].copy_from_slice(&[1, 6, 1, 0, 0, 0]);
        stamp(&mut img, 258, 6);
        stamp(&mut img, 259, 8);
        let fs = 260 * 2048;
        w32(&mut img, fs + 404, 1 + (next() % 19));
        img[fs + 408..fs + 410].copy_from_slice(&0u16.to_le_bytes());
        stamp(&mut img, 260, 256);
        if let Ok(mut u) = UdfImage::open(Cursor::new(img)) {
            opened += 1;
            for path in ["a", "a/b", "docs/x.txt"] {
                if let Ok(Some(f)) = u.find(path) {
                    let mut b = vec![0u8; (f.size as usize).min(4096)];
                    let _ = u.read_at(&f, 0, &mut b);
                }
            }
        }
    }
    assert!(opened > 250, "only {opened} images opened");
}

#[test]
fn corpus_udf_real_iso() {
    let Ok(path) = std::env::var("MILIONAR_ISO") else {
        println!("skipped: MILIONAR_ISO not set");
        return;
    };
    let mut img = UdfImage::open(std::fs::File::open(path).unwrap()).unwrap();
    let f = img.find("Setup.exe").unwrap().expect("Setup.exe");
    assert_eq!(f.size, 341_048_406);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    while pos < f.size {
        let n = ((f.size - pos) as usize).min(buf.len());
        img.read_at(&f, pos, &mut buf[..n]).unwrap();
        hasher.update(&buf[..n]);
        pos += n as u64;
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        "b2124fa4f7dd31e5763580f4ae1c9a833ac0651d6cb3c40bce7ac8fd56fa077b"
    );
    assert!(img.find("DirectX").unwrap().is_some());
}

/// A reader whose reads fail like a damaged disc.
struct FailingReader;

impl std::io::Read for FailingReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("simulated disc read error"))
    }
}

impl std::io::Seek for FailingReader {
    fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0)
    }
}

#[test]
fn udf_read_errors_stay_io_errors() {
    match UdfImage::open(FailingReader) {
        Err(FormatError::Io { source, offset, .. }) => {
            assert_eq!(offset, 256 * 2048);
            assert!(source.to_string().contains("simulated disc read error"));
        }
        Err(e) => panic!("expected Io, got {e:?}"),
        Ok(_) => panic!("expected an error"),
    }
}
