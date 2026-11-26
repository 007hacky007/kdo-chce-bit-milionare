//! Level per question (`SetLevelByQuestionNo` 0x406590, F2.2) and question selection
//! (`PickQuestion` 0x405420, F2.3).
//!
//! Each difficulty has an order array of record ids and a cursor (-1 = not built). On
//! every pick, each difficulty whose cursor is -1 gets cursor 0 and order 1..N, then its
//! `.sav` (F3) or, without a valid one, `ShuffleDraw(order, N)` on the shared RNG (easy,
//! medium, hard in that order). The pick takes order[cursor] of the current level,
//! advances the cursor and rewrites the `.sav`, or deletes it and sets the cursor to -1
//! when it reaches N. It repeats while the record's voice id equals the previous
//! question's (F11.7 KEEP: only the previous one is compared). Then `ShuffleWithInverse`
//! over the four answers.
//!
//! The order arrays hold 230 / 300 / 270 ids while the DBs have one record more, so the
//! last record of each DB is never asked (F11.1 KEEP).
//!
//! Dropped (F11.6): the loose `QuesAdio` WAV probe over all records and the per-question
//! DB rescans (the DBs are read once and cached). Added: a record id past the end of the
//! DB keeps the previous record, as the original's failed load does, and ends the repeat
//! loop; a wrap inside the repeat loop rebuilds the order (the original would index
//! order[-1]); the repeat loop gives up after 2N draws on a degenerate DB.

use std::path::Path;

use gamedata::GameData;

use super::question::{Question, Record, parse_db};
use super::saves::{delete_sav, read_sav, sav_path, write_sav};
use crate::rng::Lcg;

pub const EASY: i32 = 500;
pub const MEDIUM: i32 = 501;
pub const HARD: i32 = 502;
/// Order array lengths (easy, medium, hard).
pub const ORDER_LEN: [usize; 3] = [230, 300, 270];
pub const DB_FILES: [&str; 3] = ["Db/Mill_1.Dat", "Db/Mill_2.Dat", "Db/Mill_3.Dat"];

/// `SetLevelByQuestionNo` 0x406590 for `players` 1..4 and question number `q`.
pub fn level_for(players: usize, q: i32) -> i32 {
    if players > 1 {
        if q < 13 {
            EASY + i32::from(q > 4)
        } else {
            HARD
        }
    } else if q < 7 {
        EASY + i32::from(q > 2)
    } else {
        HARD
    }
}

/// Index 0..2 of a level, `None` for anything else.
pub fn level_index(level: i32) -> Option<usize> {
    usize::try_from(level - EASY).ok().filter(|&i| i < 3)
}

/// The selection state: static data in the original, alive for the whole program run.
#[derive(Debug, Clone)]
pub struct Selector {
    pub orders: [Vec<i32>; 3],
    /// 0x41fcc8 / 0x41fccc / 0x41fcd0; -1 = rebuild on the next pick.
    pub cursors: [i32; 3],
    /// 0x41fcd4: voice id of the previous question.
    pub prev_voice: i32,
    /// The record buffer (a failed load leaves the previous record).
    current: Record,
    dbs: [Option<Vec<Record>>; 3],
}

impl Default for Selector {
    fn default() -> Self {
        Self::new()
    }
}

impl Selector {
    pub fn new() -> Self {
        Self {
            orders: Default::default(),
            cursors: [-1; 3],
            prev_voice: -1,
            current: Record::default(),
            dbs: Default::default(),
        }
    }

    fn db(&mut self, li: usize, data: &GameData, errors: &mut Vec<String>) -> &[Record] {
        self.dbs[li].get_or_insert_with(|| match data.read(DB_FILES[li]) {
            Ok(raw) => parse_db(&raw),
            Err(e) => {
                note(errors, format!("{}: {e}", DB_FILES[li]));
                Vec::new()
            }
        })
    }
}

fn note(errors: &mut Vec<String>, e: String) {
    if errors.len() < 64 {
        errors.push(e);
    }
}

/// Cursor 0, order 1..N, then the `.sav` or `ShuffleDraw(order, N)` (the start of
/// 0x405420 for a difficulty whose cursor is -1).
fn build_order(rng: &mut Lcg, sel: &mut Selector, saves: Option<&Path>, li: usize) {
    let n = ORDER_LEN[li];
    sel.cursors[li] = 0;
    sel.orders[li] = (1..=n as i32).collect();
    match saves.and_then(|d| read_sav(&sav_path(d, li), n)) {
        Some((cursor, order)) => {
            sel.cursors[li] = cursor;
            sel.orders[li] = order;
        }
        None => rng.shuffle_draw(&mut sel.orders[li], n, None),
    }
}

/// `PickQuestion(0)` 0x405420 at `level`; `saves` = the `.sav` directory (`None`: no
/// files are read or written).
pub fn pick_question(
    rng: &mut Lcg,
    sel: &mut Selector,
    data: &GameData,
    saves: Option<&Path>,
    level: i32,
    errors: &mut Vec<String>,
) -> Question {
    for li in 0..3 {
        if sel.cursors[li] == -1 {
            build_order(rng, sel, saves, li);
        }
    }
    if let Some(li) = level_index(level) {
        let n = ORDER_LEN[li];
        for _ in 0..2 * n {
            // A wrap inside the repeat loop: rebuild as the next pick would (the
            // original indexes order[-1] here).
            if sel.cursors[li] == -1 {
                build_order(rng, sel, saves, li);
            }
            let id = sel.orders[li][sel.cursors[li] as usize];
            let rec = usize::try_from(id - 1)
                .ok()
                .and_then(|i| sel.db(li, data, errors).get(i).cloned());
            let found = rec.is_some();
            if let Some(rec) = rec {
                sel.current = rec;
            }
            sel.cursors[li] += 1;
            let wrapped = sel.cursors[li] as usize == n;
            if wrapped {
                sel.cursors[li] = -1;
            }
            let written = match saves {
                Some(dir) if wrapped => delete_sav(&sav_path(dir, li)),
                Some(dir) => write_sav(&sav_path(dir, li), sel.cursors[li], &sel.orders[li]),
                None => Ok(()),
            };
            if let Err(e) = written {
                note(errors, format!("Mill_{}.sav: {e}", li + 1));
            }
            // No record for the id (missing or short DB): keep the previous record and
            // stop, instead of consuming the whole order.
            if !found || sel.current.voice_id != sel.prev_voice {
                break;
            }
        }
    }
    sel.prev_voice = sel.current.voice_id;
    let (perm, inv, correct) = rng.shuffle_with_inverse(4);
    Question::from_record(&sel.current, &perm, &inv, correct)
}
