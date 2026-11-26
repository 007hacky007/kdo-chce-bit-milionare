//! Render-facing display list types shared by the engine and the hosts.
//!
//! All coordinates are in the original 640x480 space; the compositor scales them.

use std::hash::{Hash, Hasher};

/// Handle of a decoded Smacker file owned by the engine's asset store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetId(pub u32);

/// GDI-style rectangle: `right` and `bottom` are exclusive, as in a Win32 `RECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Arguments of GDI `CreateFontA(height, width, ...)` with the Arial face (F8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontSpec {
    /// Positive cell height (ascent + descent, internal leading included).
    pub height: i32,
    /// Average character width.
    pub width: i32,
}

/// The three `DrawText` layouts the original uses (F8), plus one for the port's own screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextLayout {
    /// Question: word-wrapped, centred, top = rect.top + (106 - h) / 2.
    QuestionCentered,
    /// Answer: word-wrapped, centred, top = rect.top + (35 - h) / 2.
    AnswerCentered,
    /// Timer: single line, centred both ways (DT_CENTER|DT_VCENTER|DT_SINGLELINE).
    SingleLineCentered,
    /// The port's own screens (never the game): word-wrapped, left-aligned lines from
    /// rect.top, clipped to rect.
    WrappedLeft,
}

/// One drawing command, applied in list order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DrawOp {
    /// Blit frame `frame` (1-based) of a Smacker asset at (x, y); `transparent` skips palette index 0.
    Smk {
        asset: AssetId,
        frame: u32,
        x: i32,
        y: i32,
        transparent: bool,
    },
    Text {
        text: String,
        font: FontSpec,
        color: [u8; 3],
        rect: Rect,
        layout: TextLayout,
    },
    Fill {
        rect: Rect,
        color: [u8; 3],
    },
}

/// A display list for one presented tick.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Frame {
    pub ops: Vec<DrawOp>,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, op: DrawOp) {
        self.ops.push(op);
    }

    /// FNV-1a hash of the display list, independent of the std hasher's seed or
    /// algorithm and of the platform's pointer width, so replay hashes are comparable.
    pub fn stable_hash(&self) -> u64 {
        let mut h = Fnv1a::default();
        self.hash(&mut h);
        h.finish()
    }
}

struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn write_usize(&mut self, i: usize) {
        self.write(&(i as u64).to_le_bytes());
    }

    fn write_isize(&mut self, i: isize) {
        self.write(&(i as i64).to_le_bytes());
    }

    fn write_u16(&mut self, i: u16) {
        self.write(&i.to_le_bytes());
    }

    fn write_u32(&mut self, i: u32) {
        self.write(&i.to_le_bytes());
    }

    fn write_u64(&mut self, i: u64) {
        self.write(&i.to_le_bytes());
    }

    fn write_i32(&mut self, i: i32) {
        self.write(&i.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> DrawOp {
        DrawOp::Text {
            text: s.to_string(),
            font: FontSpec {
                height: 30,
                width: 15,
            },
            color: [255, 255, 255],
            rect: Rect::new(58, 82, 590, 187),
            layout: TextLayout::QuestionCentered,
        }
    }

    #[test]
    fn stable_hash_distinguishes_and_repeats() {
        let a = Frame {
            ops: vec![text("a")],
        };
        let b = Frame {
            ops: vec![text("b")],
        };
        assert_eq!(a.stable_hash(), a.clone().stable_hash());
        assert_ne!(a.stable_hash(), b.stable_hash());
        assert_ne!(Frame::new().stable_hash(), a.stable_hash());
    }

    #[test]
    fn rect_geometry() {
        let r = Rect::new(58, 82, 590, 187);
        assert_eq!((r.width(), r.height()), (532, 105));
        assert!(r.contains(58, 82));
        assert!(!r.contains(590, 100));
    }
}
