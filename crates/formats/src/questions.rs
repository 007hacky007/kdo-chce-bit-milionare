//! Question database decoder (`Mill_*.dat`).
//!
//! Records are LF-separated; every byte is XORed with `0xFE`, the text is
//! windows-1250 and fields are `id|question|correct|wrong1|wrong2|wrong3|voiceId`.

use crate::{FormatError, Result};
use encoding_rs::WINDOWS_1250;

const XOR_KEY: u8 = 0xFE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: u32,
    pub text: String,
    pub correct: String,
    pub wrong: [String; 3],
    pub voice_id: u16,
}

fn invalid(index: usize, detail: &str) -> FormatError {
    FormatError::Invalid {
        what: "question record",
        detail: format!("record {index}: {detail}"),
    }
}

fn decode_record(index: usize, rec: &[u8]) -> Result<Question> {
    let plain: Vec<u8> = rec.iter().map(|b| b ^ XOR_KEY).collect();
    let (text, _) = WINDOWS_1250.decode_without_bom_handling(&plain);
    let line = text.strip_suffix('\r').unwrap_or(&text);
    let fields: Vec<&str> = line.split('|').collect();
    let [id, q, correct, w1, w2, w3, voice] = fields[..] else {
        return Err(invalid(
            index,
            &format!("expected 7 fields, found {}", fields.len()),
        ));
    };
    let id = id
        .trim()
        .parse::<u32>()
        .map_err(|_| invalid(index, "bad id"))?;
    let voice_id = voice
        .trim()
        .parse::<u16>()
        .map_err(|_| invalid(index, "bad voice id"))?;
    Ok(Question {
        id,
        text: q.to_string(),
        correct: correct.to_string(),
        wrong: [w1.to_string(), w2.to_string(), w3.to_string()],
        voice_id,
    })
}

pub fn decode_question_db(bytes: &[u8]) -> Result<Vec<Question>> {
    let mut pieces: Vec<&[u8]> = bytes.split(|&b| b == 0x0A).collect();
    if pieces.last().is_some_and(|p| p.is_empty()) {
        pieces.pop();
    }
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, rec)| decode_record(i, rec))
        .collect()
}

/// Encodes questions into the on-disk format. Meant for well-formed fixtures
/// only: fields must not contain `|`, CR or LF, nor characters that XOR to
/// 0x0A (such as 'ô'), and all text must be representable in windows-1250.
pub fn encode_question_db(qs: &[Question]) -> Vec<u8> {
    let mut out = Vec::new();
    for q in qs {
        let line = format!(
            "{}|{}|{}|{}|{}|{}|{}\r",
            q.id, q.text, q.correct, q.wrong[0], q.wrong[1], q.wrong[2], q.voice_id
        );
        let (enc, _, _) = WINDOWS_1250.encode(&line);
        let xored: Vec<u8> = enc.iter().map(|b| b ^ XOR_KEY).collect();
        debug_assert!(!xored.contains(&0x0A), "record contains raw LF after XOR");
        out.extend(xored);
        out.push(0x0A);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FormatError;

    fn sample() -> Question {
        Question {
            id: 1,
            text: "Kolik studní má vesnice Horní Žabákov?".into(),
            correct: "Sedm".into(),
            wrong: ["Pět".into(), "Devět".into(), "Tři".into()],
            voice_id: 9001,
        }
    }

    fn raw(line: &str, lf: bool) -> Vec<u8> {
        let mut v: Vec<u8> = line.bytes().map(|b| b ^ 0xFE).collect();
        if lf {
            v.push(0x0A);
        }
        v
    }

    #[test]
    fn roundtrip_czech_record() {
        let q = sample();
        let bytes = encode_question_db(&[q.clone(), Question { id: 2, ..q.clone() }]);
        assert_eq!(bytes.last(), Some(&0x0A));
        assert_eq!(bytes[bytes.len() - 2], b'\r' ^ 0xFE);
        let back = decode_question_db(&bytes).unwrap();
        assert_eq!(back, vec![q.clone(), Question { id: 2, ..q }]);
    }

    #[test]
    fn wrong_field_count_is_invalid() {
        let bytes = raw("1|q|a|b|c|d\r", true);
        match decode_question_db(&bytes) {
            Err(FormatError::Invalid { what, detail }) => {
                assert_eq!(what, "question record");
                assert!(detail.contains("record 0"), "{detail}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn bad_second_record_reports_index_one() {
        let mut bytes = raw("1|q|a|b|c|d|5\r", true);
        bytes.extend(raw("2|q|a|b|c\r", true));
        match decode_question_db(&bytes) {
            Err(FormatError::Invalid { detail, .. }) => {
                assert!(detail.contains("record 1"), "{detail}")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn non_numeric_voice_is_invalid() {
        let bytes = raw("1|q|a|b|c|d|x\r", true);
        assert!(matches!(
            decode_question_db(&bytes),
            Err(FormatError::Invalid { .. })
        ));
    }

    #[test]
    fn non_numeric_id_is_invalid() {
        let bytes = raw("z|q|a|b|c|d|5\r", true);
        assert!(matches!(
            decode_question_db(&bytes),
            Err(FormatError::Invalid { .. })
        ));
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(decode_question_db(&[]).unwrap(), vec![]);
    }

    #[test]
    fn truncated_last_record_without_lf_still_parses() {
        let bytes = raw("1|q|a|b|c|d|7\r", false);
        let qs = decode_question_db(&bytes).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].voice_id, 7);
    }
}
