use formats::questions::decode_question_db;
use gamedata::GameData;
use std::path::Path;

#[test]
fn corpus_question_counts() {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return;
    };
    let gd = GameData::open(Path::new(&root)).unwrap();
    for (name, count) in [
        ("Mill_1.dat", 231),
        ("Mill_2.dat", 301),
        ("Mill_3.dat", 271),
    ] {
        let bytes = gd.read(&format!("Db/{name}")).unwrap();
        let qs = decode_question_db(&bytes).unwrap();
        assert_eq!(qs.len(), count, "{name}");
        for (i, q) in qs.iter().enumerate() {
            assert_eq!(q.id as usize, i + 1, "{name} id order");
            assert!((1100..=2239).contains(&q.voice_id), "{name} voice {}", q.id);
        }
    }
}
