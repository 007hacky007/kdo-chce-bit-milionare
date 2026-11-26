//! `AnimAdvance` 0x40d9e0 (F1.3): the frame counter every screen steps once per tick.

/// `{start, end, cur, loops}` of the original's 0x18-byte anim struct. The interval and
/// timestamp fields are only read in pacing mode 1, which is never selected, so they are
/// omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anim {
    pub start: i32,
    pub end: i32,
    pub cur: i32,
    pub loops: i32,
}

impl Anim {
    /// An anim at its first frame.
    pub const fn new(start: i32, end: i32) -> Self {
        Self {
            start,
            end,
            cur: start,
            loops: 0,
        }
    }

    /// `cur++`; past `end` it wraps to `start` and counts a loop.
    pub fn advance(&mut self) {
        self.cur += 1;
        if self.end < self.cur {
            self.cur = self.start;
            self.loops += 1;
        }
    }

    /// The callers' "anim finished" test after an advance: `cur == start`.
    pub const fn at_start(&self) -> bool {
        self.cur == self.start
    }

    /// Restart at `start` (the original assigns `cur = start`; `loops` is kept).
    pub fn restart(&mut self) {
        self.cur = self.start;
    }

    /// Replace the range and restart (0x40d090 and its siblings).
    pub fn set(&mut self, start: i32, end: i32) {
        self.start = start;
        self.end = end;
        self.cur = start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_wraps_after_end() {
        let mut a = Anim::new(1, 3);
        let mut seen = vec![a.cur];
        for _ in 0..4 {
            a.advance();
            seen.push(a.cur);
        }
        assert_eq!(seen, [1, 2, 3, 1, 2]);
        assert_eq!(a.loops, 1);
    }

    #[test]
    fn single_frame_anim_is_finished_every_tick() {
        let mut a = Anim::new(7, 7);
        a.advance();
        assert!(a.at_start());
        assert_eq!(a.loops, 1);
    }
}
