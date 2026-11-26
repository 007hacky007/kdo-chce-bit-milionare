use formats::audiolib::{AudioLibIndex, HEADER_LEN, entry_bytes};
use formats::questions::decode_question_db;
use formats::wav::parse_wav;
use gamedata::GameData;
use std::path::Path;

fn data_root() -> Option<String> {
    let root = std::env::var("MILIONAR_DATA").ok();
    if root.is_none() {
        println!("skipped: MILIONAR_DATA not set");
    }
    root
}

fn read(gd: &GameData, dir: &str, name: &str) -> Vec<u8> {
    gd.read(&format!("{dir}/{name}")).unwrap()
}

#[test]
fn corpus_audiolib_all_entries_decode() {
    let Some(root) = data_root() else { return };
    let gd = GameData::open(Path::new(&root)).unwrap();
    for (name, count) in [("audio1.dat", 577), ("audio2.dat", 800)] {
        let file = read(&gd, "Audiolib", name);
        let idx = AudioLibIndex::parse(&file[..HEADER_LEN]).unwrap();
        assert_eq!(idx.entries.len(), count, "{name}");
        for e in &idx.entries {
            let pcm = parse_wav(entry_bytes(&file, e).unwrap())
                .unwrap_or_else(|err| panic!("{name} {}: {err}", e.name));
            assert!(!pcm.samples.is_empty(), "{name} {}", e.name);
        }
        for w in idx.entries.windows(2) {
            assert_eq!(w[0].offset + w[0].size, w[1].offset, "{name} {}", w[0].name);
        }
        let last = idx.entries.last().unwrap();
        assert_eq!(
            last.offset as usize + last.size as usize,
            file.len(),
            "{name} end"
        );
    }
}

#[test]
fn corpus_voice_ids_resolve_to_entries() {
    let Some(root) = data_root() else { return };
    let gd = GameData::open(Path::new(&root)).unwrap();
    let idxs: Vec<AudioLibIndex> = ["audio1.dat", "audio2.dat"]
        .iter()
        .map(|n| {
            let file = read(&gd, "Audiolib", n);
            AudioLibIndex::parse(&file[..HEADER_LEN]).unwrap()
        })
        .collect();
    let mut missing = Vec::new();
    for db in ["Mill_1.dat", "Mill_2.dat", "Mill_3.dat"] {
        for q in decode_question_db(&read(&gd, "Db", db)).unwrap() {
            let plain = format!("Char{:04}.wav", q.voice_id);
            let ques = format!("Char{:04}q.wav", q.voice_id);
            let hit = idxs
                .iter()
                .any(|i| i.find(&plain).is_some() || i.find(&ques).is_some());
            if !hit {
                missing.push(format!("{db}#{} voice {}", q.id, q.voice_id));
            }
        }
    }
    assert!(missing.is_empty(), "unresolved: {missing:?}");
}
