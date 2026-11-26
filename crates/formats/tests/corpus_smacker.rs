use formats::smacker::{AudioCodec, Smacker};
use gamedata::GameData;
use std::path::{Path, PathBuf};
use std::process::Command;

fn data_root() -> Option<String> {
    let root = std::env::var("MILIONAR_DATA").ok();
    if root.is_none() {
        println!("skipped: MILIONAR_DATA not set");
    }
    root
}

fn smk_files(root: &str) -> Vec<PathBuf> {
    let gd = GameData::open(Path::new(root)).unwrap();
    let mut out: Vec<PathBuf> = gd
        .files()
        .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("smk")))
        .map(Path::to_path_buf)
        .collect();
    out.sort();
    out
}

fn tool_available(name: &str) -> bool {
    Command::new(name).arg("-version").output().is_ok()
}

/// Returns (width, height, video packet count) as reported by ffprobe.
fn ffprobe(path: &Path) -> (u32, u32, u32) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets"])
        .args(["-show_entries", "stream=width,height,nb_read_packets"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "ffprobe failed on {path:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    let v: Vec<u32> = text.trim().split(',').map(|s| s.parse().unwrap()).collect();
    (v[0], v[1], v[2])
}

fn ffmpeg_frame(path: &Path, k: u32) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &format!("select=eq(n\\,{k})"), "-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ffmpeg failed on {path:?} frame {k}");
    out.stdout
}

#[test]
fn corpus_smacker_matches_ffmpeg() {
    let Some(root) = data_root() else { return };
    let files = smk_files(&root);
    assert!(!files.is_empty(), "no .smk files under {root}/game");
    let ffmpeg = tool_available("ffmpeg") && tool_available("ffprobe");
    if !ffmpeg {
        println!("note: ffmpeg/ffprobe not on PATH, comparing decode only");
    }
    let mut total = 0u32;
    let mut compared = 0u32;
    let start = std::time::Instant::now();
    let mut ffmpeg_time = std::time::Duration::ZERO;
    for path in &files {
        let smk = Smacker::parse(std::fs::read(path).unwrap())
            .unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let h = smk.header().clone();
        total += h.frames;
        if ffmpeg {
            assert_eq!(ffprobe(path), (h.width, h.height, h.frames), "{path:?}");
        }
        let n = h.frames;
        let checks = [0, n / 2, n.saturating_sub(1)];
        let mut dec = smk.decoder();
        let mut decoded = 0;
        while let Some(f) = dec
            .next_frame()
            .unwrap_or_else(|e| panic!("{path:?} frame {decoded}: {e}"))
        {
            assert_eq!(f.index, decoded);
            assert_eq!(f.pixels.len(), (h.width * h.height) as usize);
            if ffmpeg && checks.contains(&f.index) {
                let rgb: Vec<u8> = f
                    .pixels
                    .iter()
                    .flat_map(|&p| f.palette[p as usize])
                    .collect();
                let t = std::time::Instant::now();
                let want = ffmpeg_frame(path, f.index);
                ffmpeg_time += t.elapsed();
                compared += 1;
                assert_eq!(rgb.len(), want.len(), "{path:?} frame {}", f.index);
                if rgb != want {
                    let first = rgb.iter().zip(&want).position(|(a, b)| a != b).unwrap();
                    panic!(
                        "{path:?} frame {} differs at byte {first}: ours {:?} ffmpeg {:?}",
                        f.index,
                        &rgb[first..(first + 6).min(rgb.len())],
                        &want[first..(first + 6).min(want.len())]
                    );
                }
            }
            decoded += 1;
        }
        assert_eq!(decoded, n, "{path:?}");
    }
    println!(
        "{} files, {total} frames decoded, {compared} compared with ffmpeg; {:.2?} excluding ffmpeg",
        files.len(),
        start.elapsed() - ffmpeg_time
    );
    assert_eq!(total, 3235);
}

/// Decodes every frame, including audio, until the end or the first error.
fn decode_all(bytes: Vec<u8>) {
    let Ok(smk) = Smacker::parse(bytes) else {
        return;
    };
    let mut dec = smk.decoder();
    while let Ok(Some(f)) = dec.next_frame() {
        std::hint::black_box(f.audio.len());
    }
}

#[test]
fn smacker_truncation_never_panics() {
    let Some(root) = data_root() else { return };
    let gd = GameData::open(Path::new(&root)).unwrap();
    // A silent file plus one DPCM and one Bink audio file. The audio files
    // are large, so only their first 400 KB (header, tables and a few dozen
    // frames with audio) is mutated, with fewer rounds, to keep debug builds
    // fast: (name, bytes kept, truncation steps, corruption rounds).
    for (name, keep, steps, rounds) in [
        ("Menus/Mm_dbut.smk", usize::MAX, 64, 256),
        ("Cuts/End_01.smk", 400_000, 32, 96),
        ("Menus/Opening.smk", 400_000, 32, 96),
    ] {
        let mut full = gd.read(name).unwrap();
        // The untruncated file must decode cleanly.
        let smk = Smacker::parse(full.clone()).unwrap();
        let mut dec = smk.decoder();
        while dec.next_frame().unwrap().is_some() {}
        drop(dec);
        drop(smk);
        full.truncate(keep);

        for i in 0..steps {
            let len = full.len() * i / steps;
            let bytes = full[..len].to_vec();
            let r = std::panic::catch_unwind(move || decode_all(bytes));
            assert!(r.is_ok(), "{name}: panic when truncated to {len} bytes");
        }

        // Also flip bytes at pseudo-random positions (fixed LCG seed).
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        for round in 0..rounds {
            let mut bytes = full.clone();
            for _ in 0..1 + round % 4 {
                let at = next() as usize % bytes.len();
                bytes[at] ^= (next() >> 24) as u8 | 1;
            }
            let r = std::panic::catch_unwind(move || decode_all(bytes));
            assert!(r.is_ok(), "{name}: panic in corruption round {round}");
        }
    }
}

/// The files with an audio track: (path under `game/`, rate, channels, codec).
const AUDIO_FILES: [(&str, u32, u16, AudioCodec); 8] = [
    ("Cuts/End_01.smk", 22050, 1, AudioCodec::SmackerDpcm),
    ("Cuts/End_02.smk", 22050, 1, AudioCodec::SmackerDpcm),
    ("Cuts/End_03.smk", 22050, 1, AudioCodec::SmackerDpcm),
    ("Cuts/End_04.smk", 22050, 1, AudioCodec::SmackerDpcm),
    ("Cuts/End_05.smk", 22050, 1, AudioCodec::SmackerDpcm),
    ("Cuts/Logos.smk", 44100, 2, AudioCodec::BinkRdft),
    ("Cuts/Mm_open.smk", 22050, 1, AudioCodec::BinkRdft),
    ("Menus/Opening.smk", 22050, 1, AudioCodec::BinkRdft),
];

fn rel_name(root: &str, path: &Path) -> String {
    let game: PathBuf = [root, "game"].iter().collect();
    let rel = path.strip_prefix(&game).unwrap();
    rel.to_string_lossy().replace('\\', "/")
}

#[test]
fn corpus_smacker_audio_tracks() {
    let Some(root) = data_root() else { return };
    let mut with_audio = Vec::new();
    for path in smk_files(&root) {
        let smk = Smacker::parse(std::fs::read(&path).unwrap()).unwrap();
        let h = smk.header();
        assert!(h.audio[1..].iter().all(Option::is_none), "{path:?}");
        if let Some(a) = h.audio[0] {
            with_audio.push((rel_name(&root, &path), a.sample_rate, a.channels, a.codec));
        }
    }
    let want: Vec<_> = AUDIO_FILES
        .iter()
        .map(|&(p, r, c, k)| (p.to_string(), r, c, k))
        .collect();
    assert_eq!(with_audio, want);
}

fn ffmpeg_audio(path: &Path) -> Vec<i16> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "s16le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ffmpeg failed on {path:?}");
    out.stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect()
}

#[test]
fn corpus_smacker_audio_matches_ffmpeg() {
    let Some(root) = data_root() else { return };
    let gd = GameData::open(Path::new(&root)).unwrap();
    if !tool_available("ffmpeg") {
        println!("skipped: ffmpeg not on PATH");
        return;
    }
    for (name, rate, channels, codec) in AUDIO_FILES {
        let path = gd.resolve(name).unwrap();
        let smk = Smacker::parse(std::fs::read(&path).unwrap()).unwrap();
        let info = smk.header().audio[0].unwrap();
        assert_eq!(
            (info.sample_rate, info.channels, info.codec),
            (rate, channels, codec)
        );
        let mut ours = Vec::new();
        let mut dec = smk.decoder();
        while let Some(f) = dec.next_frame().unwrap() {
            for c in f.audio.iter().filter(|c| c.track == 0) {
                ours.extend_from_slice(&c.samples);
            }
        }
        let want = ffmpeg_audio(&path);
        match codec {
            AudioCodec::BinkRdft => {
                // Lengths match exactly; a lost block must fail the test.
                assert_eq!(ours.len(), want.len(), "{name}: sample count");
                let n = ours.len().min(want.len());
                let mut max = 0i32;
                let mut sq = 0f64;
                for (a, b) in ours[..n].iter().zip(&want[..n]) {
                    let d = (i32::from(*a) - i32::from(*b)).abs();
                    max = max.max(d);
                    sq += f64::from(d * d);
                }
                let rms = (sq / n.max(1) as f64).sqrt();
                println!("{name}: {n} samples, max diff {max}, rms {rms:.4}");
                assert!(n > 0, "{name}: no audio decoded");
                assert!(max <= 2, "{name}: max diff {max} LSB, rms {rms:.4}");
            }
            _ => {
                assert_eq!(ours.len(), want.len(), "{name}: sample count");
                if let Some(i) = ours.iter().zip(&want).position(|(a, b)| a != b) {
                    panic!(
                        "{name}: first mismatch at sample {i}: {} vs {}",
                        ours[i], want[i]
                    );
                }
            }
        }
    }
}
