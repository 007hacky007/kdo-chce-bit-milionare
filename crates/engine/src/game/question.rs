//! The question record `g_curQuestion` 0x425380 and the question text (F2.1, F8).
//!
//! DB framing as `ReadDbLine` 0x40c640 / `XorFE_Line` 0x40c620 / `ReadPipeField` 0x40c6c0
//! read it: `fgets(buf, 0x3ff)` pieces (a raw LF ends one; a longer line is split into
//! pieces of 1022 bytes), every byte before the raw LF XORed with 0xFE; a piece whose
//! first decoded byte is CR is skipped and one whose first decoded byte is NUL ends the
//! file. Record N is the N-th remaining piece (1-based). Fields are split at `|` and end
//! at CR or LF; each is limited to its buffer (id 256, question 256, answers 64, voice
//! 63 bytes; a longer field continues as the next one) and loses trailing spaces. The
//! voice id is `atoi` of field 7. Text is cp1250.
//!
//! Text (F8): question 30 x 15 white in (58,82)-(590,187); answers 20 x 10, white for
//! state 1, grey 0x808080 for state 0, not drawn for -1, "A. " + answer in the 0x41fcd8
//! rects; timers 20 x 15 white "%d" of timer / 15 in 32 x 24 boxes at (4,172) and
//! (604,172).

use encoding_rs::WINDOWS_1250;
use render::{DrawOp, FontSpec, Rect, TextLayout};

use crate::ctx::Ctx;

/// fgets buffer size (`0x3ff` bytes including the NUL).
const FGETS_CHARS: usize = 0x3fe;
const XOR_KEY: u8 = 0xFE;

/// One DB record as the original keeps it: question, answers in DB order (0 = correct)
/// and the voice id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub text: String,
    pub answers: [String; 4],
    pub voice_id: i32,
}

/// `fgets` + `XorFE_Line` + the skip rules of `ReadDbLine`: the decoded record pieces.
pub fn db_records(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        let rest = &raw[pos..];
        let len = match rest.iter().position(|&b| b == b'\n') {
            Some(i) if i < FGETS_CHARS => i + 1,
            _ => rest.len().min(FGETS_CHARS),
        };
        pos += len;
        let piece: Vec<u8> = rest[..len]
            .iter()
            .map(|&b| if b == b'\n' { b } else { b ^ XOR_KEY })
            .collect();
        match piece.first() {
            None | Some(0) => break,
            Some(b'\r') => continue,
            Some(_) => out.push(piece),
        }
    }
    out
}

/// `ReadPipeField` 0x40c6c0 over `buf[pos..]`: at most `n` bytes up to `|`, CR, LF or
/// NUL; a `|` is consumed, a limit stop is not. Trailing spaces are stripped.
fn field(buf: &[u8], pos: &mut usize, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(&c) = buf.get(*pos) {
        match c {
            b'\n' | b'\r' | 0 => break,
            b'|' => {
                *pos += 1;
                break;
            }
            _ => {
                out.push(c);
                *pos += 1;
                if out.len() == n {
                    break;
                }
            }
        }
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    out
}

/// MSVC `atoi`: leading white space, an optional sign, then digits (wrapping).
fn atoi(s: &[u8]) -> i32 {
    let mut i = 0;
    while s.get(i).is_some_and(|c| matches!(c, b' ' | b'\t'..=b'\r')) {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut v: i32 = 0;
    while let Some(d) = s.get(i).filter(|c| c.is_ascii_digit()) {
        v = v.wrapping_mul(10).wrapping_add(i32::from(d - b'0'));
        i += 1;
    }
    if neg { v.wrapping_neg() } else { v }
}

fn cp1250(b: &[u8]) -> String {
    WINDOWS_1250.decode_without_bom_handling(b).0.into_owned()
}

/// The fields of one decoded piece (`LoadQuestionRecord` 0x405950).
pub fn parse_record(piece: &[u8]) -> Record {
    let mut pos = 0;
    let _id = field(piece, &mut pos, 0x100);
    let text = cp1250(&field(piece, &mut pos, 0x100));
    let answers = std::array::from_fn(|_| cp1250(&field(piece, &mut pos, 0x40)));
    let voice_id = atoi(&field(piece, &mut pos, 0x3f));
    Record {
        text,
        answers,
        voice_id,
    }
}

/// A whole DB file decoded into records (record N = index N-1).
pub fn parse_db(raw: &[u8]) -> Vec<Record> {
    db_records(raw).iter().map(|p| parse_record(p)).collect()
}

/// `g_curQuestion` with its display state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    /// Answers in DB order; 0 is the correct one.
    pub answers: [String; 4],
    pub voice_id: i32,
    /// Per display slot: 1 normal, 0 greyed, -1 hidden (+0x200).
    pub states: [i32; 4],
    /// Display slot of the correct answer (+0x214).
    pub correct_slot: usize,
    /// perm[slot] = DB answer shown in that slot (+0x218).
    pub perm: [usize; 4],
    /// inv[answer] = slot showing that DB answer (+0x228).
    pub inv: [usize; 4],
}

impl Default for Question {
    fn default() -> Self {
        Self {
            text: String::new(),
            answers: Default::default(),
            voice_id: 0,
            states: [1; 4],
            correct_slot: 0,
            perm: [0, 1, 2, 3],
            inv: [0, 1, 2, 3],
        }
    }
}

impl Question {
    /// A record with the answer order of `ShuffleWithInverse` (perm, inv, correct slot).
    pub fn from_record(rec: &Record, perm: &[usize], inv: &[usize], correct: usize) -> Self {
        let mut q = Self {
            text: rec.text.clone(),
            answers: rec.answers.clone(),
            voice_id: rec.voice_id,
            correct_slot: correct,
            ..Self::default()
        };
        q.perm.copy_from_slice(&perm[..4]);
        q.inv.copy_from_slice(&inv[..4]);
        q
    }

    /// An empty record with the given answer order (tests).
    pub fn synthetic(perm: [usize; 4]) -> Self {
        let mut inv = [0; 4];
        for (slot, &a) in perm.iter().enumerate() {
            inv[a] = slot;
        }
        Self {
            perm,
            inv,
            correct_slot: inv[0],
            ..Self::default()
        }
    }

    /// Slot state for a possibly out-of-range index (`chosen` = 4 is the N button).
    pub fn state(&self, slot: i32) -> i32 {
        usize::try_from(slot)
            .ok()
            .and_then(|s| self.states.get(s).copied())
            .unwrap_or(0)
    }
}

pub const QUESTION_RECT: Rect = Rect::new(58, 82, 590, 187);
pub const QUESTION_FONT: FontSpec = FontSpec {
    height: 30,
    width: 15,
};
/// Answer rects (0x41fcd8).
pub const ANSWER_RECTS: [Rect; 4] = [
    Rect::new(50, 217, 589, 251),
    Rect::new(50, 267, 589, 301),
    Rect::new(50, 317, 589, 351),
    Rect::new(50, 367, 589, 401),
];
pub const ANSWER_FONT: FontSpec = FontSpec {
    height: 20,
    width: 10,
};
pub const TIMER_FONT: FontSpec = FontSpec {
    height: 20,
    width: 15,
};
/// The two timer boxes: 0x410190 calls `SetRect(x, y, x - 1 + 32, y - 1 + 24)` for (4,172)
/// and (604,172), so the rects are one pixel short of 32 x 24.
pub const TIMER_RECTS: [Rect; 2] = [Rect::new(4, 172, 35, 195), Rect::new(604, 172, 635, 195)];
/// Answer prefixes (0x41fd18).
pub const PREFIXES: [&str; 4] = ["A. ", "B. ", "C. ", "D. "];
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];
const GREY: [u8; 3] = [0x80, 0x80, 0x80];

/// `DrawQuestionText` 0x407c80.
pub fn draw_question(ctx: &mut Ctx, q: &Question) {
    ctx.draw(DrawOp::Text {
        text: q.text.clone(),
        font: QUESTION_FONT,
        color: WHITE,
        rect: QUESTION_RECT,
        layout: TextLayout::QuestionCentered,
    });
}

/// `DrawAnswerText` 0x407d50 for one display slot.
pub fn draw_answer(ctx: &mut Ctx, q: &Question, slot: usize) {
    let color = match q.states[slot] {
        -1 => return,
        1 => WHITE,
        _ => GREY,
    };
    ctx.draw(DrawOp::Text {
        text: format!("{}{}", PREFIXES[slot], q.answers[q.perm[slot]]),
        font: ANSWER_FONT,
        color,
        rect: ANSWER_RECTS[slot],
        layout: TextLayout::AnswerCentered,
    });
}

/// The four answers.
pub fn draw_answers(ctx: &mut Ctx, q: &Question) {
    for slot in 0..4 {
        draw_answer(ctx, q, slot);
    }
}

/// The two timer boxes showing `ticks / 15` (0x407ae0 / 0x407bb0).
pub fn draw_timer(ctx: &mut Ctx, ticks: i32) {
    for rect in TIMER_RECTS {
        ctx.draw(DrawOp::Text {
            text: (ticks / 15).to_string(),
            font: TIMER_FONT,
            color: WHITE,
            rect,
            layout: TextLayout::SingleLineCentered,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0x410190 draws the timer in `SetRect(x, y, x + 31, y + 23)`; with the old 32 x 24
    /// rect the digits sat one pixel right and low of the reference capture.
    #[test]
    fn timer_rects_match_setrect() {
        for (rect, x) in TIMER_RECTS.iter().zip([4, 604]) {
            assert_eq!(*rect, Rect::new(x, 172, x - 1 + 32, 172 - 1 + 24));
        }
    }

    fn enc(s: &[u8]) -> Vec<u8> {
        s.iter()
            .map(|&b| if b == b'\n' { b } else { b ^ XOR_KEY })
            .collect()
    }

    #[test]
    fn framing_skips_cr_lines_and_stops_at_nul() {
        let mut raw = enc(b"1|Q one|a|b|c|d|12\r\n");
        raw.extend(enc(b"\r\n"));
        raw.extend(enc(b"2|Q two  |a2|b2|c2|d2|  7\r\n"));
        raw.extend(enc(b"\0|x\n"));
        raw.extend(enc(b"3|never|a|b|c|d|1\n"));
        let db = parse_db(&raw);
        assert_eq!(db.len(), 2);
        assert_eq!(db[0].text, "Q one");
        assert_eq!(db[0].voice_id, 12);
        assert_eq!(db[1].text, "Q two");
        assert_eq!(db[1].answers, ["a2", "b2", "c2", "d2"].map(String::from));
        assert_eq!(db[1].voice_id, 7);
    }

    #[test]
    fn overlong_answer_continues_as_next_field() {
        let long = "x".repeat(70);
        let line = format!("1|q|{long}|b|c|d|5\n");
        let db = parse_db(&enc(line.as_bytes()));
        assert_eq!(db[0].answers[0], "x".repeat(64));
        assert_eq!(db[0].answers[1], "x".repeat(6));
        assert_eq!(db[0].answers[2], "b");
    }

    #[test]
    fn empty_line_is_a_record() {
        let mut raw = b"\n".to_vec();
        raw.extend(enc(b"1|q|a|b|c|d|5\n"));
        let db = parse_db(&raw);
        assert_eq!(db.len(), 2);
        assert_eq!(db[0], Record::default());
    }
}
