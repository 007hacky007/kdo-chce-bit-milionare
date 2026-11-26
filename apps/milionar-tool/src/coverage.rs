//! Decodes the whole imported corpus and compares against the pinned counts.

use formats::audiolib::{AudioLibIndex, HEADER_LEN, entry_bytes};
use formats::questions::decode_question_db;
use formats::smacker::Smacker;
use formats::wav::parse_wav;
use gamedata::GameData;

const QUESTIONS: [(&str, usize); 3] = [
    ("Mill_1.dat", 231),
    ("Mill_2.dat", 301),
    ("Mill_3.dat", 271),
];
const AUDIO: [(&str, usize); 2] = [("audio1.dat", 577), ("audio2.dat", 800)];
const SMK_FILES: usize = 80;
const SMK_FRAMES: u64 = 3235;
const SMK_AUDIO_FILES: usize = 8;

#[derive(Default)]
pub struct Report {
    pub questions: [usize; 3],
    pub audio: [usize; 2],
    pub smk_files: usize,
    pub frames: u64,
    pub audio_files: usize,
    /// Decode failures, one line each.
    pub errors: Vec<String>,
}

pub fn run(gd: &GameData) -> Report {
    let mut r = Report::default();
    for (i, (name, _)) in QUESTIONS.iter().enumerate() {
        match gd
            .read(&format!("Db/{name}"))
            .map_err(|e| e.to_string())
            .and_then(|b| decode_question_db(&b).map_err(|e| e.to_string()))
        {
            Ok(qs) => r.questions[i] = qs.len(),
            Err(e) => r.errors.push(format!("{name}: {e}")),
        }
    }
    for (i, (name, _)) in AUDIO.iter().enumerate() {
        match audio_entries(gd, name, &mut r.errors) {
            Ok(n) => r.audio[i] = n,
            Err(e) => r.errors.push(format!("{name}: {e}")),
        }
    }
    let mut smk: Vec<_> = gd
        .files()
        .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("smk")))
        .collect();
    smk.sort();
    for path in smk {
        let label = path.strip_prefix(gd.game_dir()).unwrap_or(path).display();
        match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| Smacker::parse(b).map_err(|e| e.to_string()))
        {
            Ok(file) => {
                r.smk_files += 1;
                let mut has_audio = false;
                let mut dec = file.decoder();
                loop {
                    match dec.next_frame() {
                        Ok(Some(f)) => {
                            has_audio |= f.audio.iter().any(|a| !a.samples.is_empty());
                            r.frames += 1;
                        }
                        Ok(None) => break,
                        Err(e) => {
                            r.errors.push(format!("{label}: {e}"));
                            break;
                        }
                    }
                }
                r.audio_files += usize::from(has_audio);
            }
            Err(e) => r.errors.push(format!("{label}: {e}")),
        }
    }
    r
}

/// Decodes every entry of one archive; returns the number that decoded.
fn audio_entries(gd: &GameData, name: &str, errors: &mut Vec<String>) -> Result<usize, String> {
    let file = gd
        .read(&format!("Audiolib/{name}"))
        .map_err(|e| e.to_string())?;
    let head = file.get(..HEADER_LEN).ok_or("truncated header")?;
    let idx = AudioLibIndex::parse(head).map_err(|e| e.to_string())?;
    let mut ok = 0;
    for e in &idx.entries {
        match entry_bytes(&file, e).and_then(parse_wav) {
            Ok(pcm) if !pcm.samples.is_empty() => ok += 1,
            Ok(_) => errors.push(format!("{name} {}: no samples", e.name)),
            Err(err) => errors.push(format!("{name} {}: {err}", e.name)),
        }
    }
    Ok(ok)
}

impl Report {
    pub fn to_text(&self) -> String {
        format!(
            "questions {}/{}/{}\naudio {}/{}\nsmacker files {} frames {} audio_files {}\n",
            self.questions[0],
            self.questions[1],
            self.questions[2],
            self.audio[0],
            self.audio[1],
            self.smk_files,
            self.frames,
            self.audio_files
        )
    }

    pub fn to_json(&self) -> String {
        serde_json::json!({
            "questions": self.questions,
            "audio": self.audio,
            "smacker": {
                "files": self.smk_files,
                "frames": self.frames,
                "audio_files": self.audio_files,
            },
            "errors": self.errors,
            "ok": self.problems().is_empty(),
        })
        .to_string()
    }

    /// Decode failures and count mismatches; empty means success.
    pub fn problems(&self) -> Vec<String> {
        let mut p = self.errors.clone();
        for (i, (name, want)) in QUESTIONS.iter().enumerate() {
            if self.questions[i] != *want {
                p.push(format!(
                    "{name}: {} records, expected {want}",
                    self.questions[i]
                ));
            }
        }
        for (i, (name, want)) in AUDIO.iter().enumerate() {
            if self.audio[i] != *want {
                p.push(format!(
                    "{name}: {} entries, expected {want}",
                    self.audio[i]
                ));
            }
        }
        if self.smk_files != SMK_FILES {
            p.push(format!(
                "{} smacker files, expected {SMK_FILES}",
                self.smk_files
            ));
        }
        if self.frames != SMK_FRAMES {
            p.push(format!(
                "{} smacker frames, expected {SMK_FRAMES}",
                self.frames
            ));
        }
        if self.audio_files != SMK_AUDIO_FILES {
            p.push(format!(
                "{} smacker audio files, expected {SMK_AUDIO_FILES}",
                self.audio_files
            ));
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Report {
        Report {
            questions: [231, 301, 271],
            audio: [577, 800],
            smk_files: 80,
            frames: 3235,
            audio_files: 8,
            errors: Vec::new(),
        }
    }

    #[test]
    fn pinned_counts_have_no_problems() {
        assert!(good().problems().is_empty());
    }

    #[test]
    fn mismatched_counts_are_reported() {
        let r = Report {
            questions: [230, 301, 271],
            audio: [577, 799],
            smk_files: 81,
            frames: 3234,
            audio_files: 7,
            errors: vec!["x.smk: bad".into()],
        };
        let p = r.problems();
        assert_eq!(p.len(), 6);
        assert!(p[0].contains("x.smk"));
        assert!(p.iter().any(|s| s.contains("Mill_1.dat")));
        assert!(p.iter().any(|s| s.contains("audio2.dat")));
        assert!(p.iter().any(|s| s.contains("81 smacker files")));
        assert!(p.iter().any(|s| s.contains("3234 smacker frames")));
        assert!(p.iter().any(|s| s.contains("7 smacker audio files")));
    }
}
