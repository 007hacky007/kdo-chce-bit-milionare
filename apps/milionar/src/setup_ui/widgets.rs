//! Layout geometry and drawing helpers of the setup screens.
//!
//! Everything is in the 640x480 logical space of a game frame; the compositor draws it
//! at the window's scale (text is rasterised at that scale, so it stays sharp). The
//! look is neutral (dark background, light text, one accent colour) and uses no game art.

use render::{DrawOp, Font, FontSpec, Frame, Rect, TextLayout, layout_text};

pub const BG: [u8; 3] = [16, 16, 24];
pub const PANEL_BG: [u8; 3] = [28, 29, 42];
pub const PANEL_EDGE: [u8; 3] = [64, 66, 92];
pub const TEXT: [u8; 3] = [238, 238, 242];
pub const DIM: [u8; 3] = [160, 162, 182];
pub const ACCENT: [u8; 3] = [240, 176, 48];
pub const WARN: [u8; 3] = [255, 150, 120];
const BUTTON_BG: [u8; 3] = [46, 48, 68];
const BUTTON_FOCUS_TEXT: [u8; 3] = [20, 20, 28];
const BAR_TRACK: [u8; 3] = [50, 52, 74];

/// The panel all content sits in.
pub const PANEL: Rect = Rect::new(24, 24, 616, 456);
const LEFT: i32 = 48;
const RIGHT: i32 = 592;
const TITLE_TOP: i32 = 44;
const TITLE_FONT: FontSpec = FontSpec {
    height: 28,
    width: 0,
};
pub const BODY_FONT: FontSpec = FontSpec {
    height: 18,
    width: 0,
};
const SMALL_FONT: FontSpec = FontSpec {
    height: 15,
    width: 0,
};
const PARA_GAP: i32 = 8;
/// Ends a paragraph cut short for lack of room (distinct from "..." in shortened values).
pub const ELLIPSIS: char = '\u{2026}';
const BUTTON_W: i32 = 400;
const BUTTON_H: i32 = 30;
const BUTTON_GAP: i32 = 8;
/// Bottom edge of the lowest button (the key hint goes below).
const BUTTONS_BOTTOM: i32 = PANEL.bottom - 36;
const HINT_TOP: i32 = PANEL.bottom - 26;

/// Where `n` buttons go: a centred column at the bottom of the panel, first on top.
pub fn button_rects(n: usize) -> Vec<Rect> {
    let left = (PANEL.left + PANEL.right - BUTTON_W) / 2;
    (0..n)
        .map(|i| {
            let from_bottom = (n - i) as i32;
            let bottom = BUTTONS_BOTTOM - (from_bottom - 1) * (BUTTON_H + BUTTON_GAP);
            Rect::new(left, bottom - BUTTON_H, left + BUTTON_W, bottom)
        })
        .collect()
}

/// The button under (x, y), if any.
pub fn hit(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects.iter().position(|r| r.contains(x, y))
}

/// Builds one screen top to bottom.
pub struct Page<'f> {
    font: &'f Font,
    frame: Frame,
    /// Top of the next paragraph.
    y: i32,
    /// Lowest y the body may use (top of the buttons).
    limit: i32,
}

impl<'f> Page<'f> {
    /// Background, panel and title; the body ends above `buttons` buttons.
    pub fn new(font: &'f Font, title: &str, buttons: usize) -> Page<'f> {
        let mut frame = Frame::new();
        frame.push(DrawOp::Fill {
            rect: Rect::new(0, 0, 640, 480),
            color: BG,
        });
        frame.push(DrawOp::Fill {
            rect: PANEL,
            color: PANEL_EDGE,
        });
        frame.push(DrawOp::Fill {
            rect: Rect::new(
                PANEL.left + 1,
                PANEL.top + 1,
                PANEL.right - 1,
                PANEL.bottom - 1,
            ),
            color: PANEL_BG,
        });
        let limit = button_rects(buttons)
            .first()
            .map_or(HINT_TOP, |r| r.top - PARA_GAP);
        let mut page = Page {
            font,
            frame,
            y: TITLE_TOP,
            limit,
        };
        page.text(title, TITLE_FONT, ACCENT, 6);
        // An accent rule under the title.
        page.frame.push(DrawOp::Fill {
            rect: Rect::new(LEFT, page.y, RIGHT, page.y + 2),
            color: ACCENT,
        });
        page.y += 2 + 14;
        page
    }

    /// A word-wrapped paragraph in the body font.
    pub fn para(&mut self, text: &str, color: [u8; 3]) {
        self.text(text, BODY_FONT, color, PARA_GAP);
    }

    /// A paragraph in the small font.
    pub fn small(&mut self, text: &str, color: [u8; 3]) {
        self.text(text, SMALL_FONT, color, PARA_GAP);
    }

    /// Wrapped text at the current y, as many lines as fit above the buttons.
    fn text(&mut self, text: &str, font: FontSpec, color: [u8; 3], gap: i32) {
        if text.is_empty() {
            return;
        }
        let probe = Rect::new(LEFT, self.y, RIGHT, i32::MAX / 2);
        let laid = layout_text(self.font, text, font, probe, TextLayout::WrappedLeft);
        let cell = laid.scale.cell_height();
        let room = ((self.limit - self.y) / cell.max(1)).max(0) as usize;
        let lines = laid.lines.len().min(room);
        if lines == 0 {
            return;
        }
        let bottom = self.y + cell * lines as i32;
        let shown = if lines < laid.lines.len() {
            self.truncated(&laid.lines[..lines], font, probe)
        } else {
            text.to_string()
        };
        self.frame.push(DrawOp::Text {
            text: shown,
            font,
            color,
            rect: Rect::new(LEFT, self.y, RIGHT, bottom),
            layout: TextLayout::WrappedLeft,
        });
        self.y = bottom + gap;
    }

    /// The lines that fit, the last one ending in an ellipsis (shortened until it fits
    /// on its line). Never expected with the real strings; a safety net for long paths.
    fn truncated(&self, kept: &[render::LaidLine], font: FontSpec, probe: Rect) -> String {
        let mut rows: Vec<String> = kept.iter().map(|l| l.text.clone()).collect();
        if let Some(last) = rows.last_mut() {
            loop {
                let candidate = format!("{}{ELLIPSIS}", last.trim_end());
                let fits = layout_text(self.font, &candidate, font, probe, TextLayout::WrappedLeft)
                    .lines
                    .len()
                    == 1;
                if fits || last.is_empty() {
                    *last = candidate;
                    break;
                }
                last.pop();
            }
        }
        rows.join("\n")
    }

    /// A progress bar (0.0..=1.0).
    pub fn bar(&mut self, fraction: f64) {
        let h = 18;
        if self.y + h > self.limit {
            return;
        }
        let track = Rect::new(LEFT, self.y, RIGHT, self.y + h);
        self.frame.push(DrawOp::Fill {
            rect: track,
            color: BAR_TRACK,
        });
        let w = (f64::from(track.width() - 4) * fraction.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            self.frame.push(DrawOp::Fill {
                rect: Rect::new(LEFT + 2, self.y + 2, LEFT + 2 + w, self.y + h - 2),
                color: ACCENT,
            });
        }
        self.y += h + PARA_GAP + 4;
    }

    /// A thin progress bar right under the previous line (one per upscale phase).
    pub fn thin_bar(&mut self, fraction: f64) {
        let h = 8;
        let y = self.y - PARA_GAP + 2;
        if y + h > self.limit {
            return;
        }
        let track = Rect::new(LEFT, y, RIGHT, y + h);
        self.frame.push(DrawOp::Fill {
            rect: track,
            color: BAR_TRACK,
        });
        let w = (f64::from(track.width() - 2) * fraction.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            self.frame.push(DrawOp::Fill {
                rect: Rect::new(LEFT + 1, y + 1, LEFT + 1 + w, y + h - 1),
                color: ACCENT,
            });
        }
        self.y = y + h + PARA_GAP + 2;
    }

    /// The button column (labels in order) with `focus` highlighted, and the key hint.
    pub fn finish(mut self, labels: &[&str], focus: usize, hint: &str) -> Frame {
        for (i, (rect, label)) in button_rects(labels.len()).iter().zip(labels).enumerate() {
            let focused = i == focus;
            if focused {
                self.frame.push(DrawOp::Fill {
                    rect: *rect,
                    color: ACCENT,
                });
            } else {
                self.frame.push(DrawOp::Fill {
                    rect: *rect,
                    color: PANEL_EDGE,
                });
                self.frame.push(DrawOp::Fill {
                    rect: Rect::new(rect.left + 1, rect.top + 1, rect.right - 1, rect.bottom - 1),
                    color: BUTTON_BG,
                });
            }
            self.frame.push(DrawOp::Text {
                text: (*label).to_string(),
                font: BODY_FONT,
                color: if focused { BUTTON_FOCUS_TEXT } else { TEXT },
                rect: *rect,
                layout: TextLayout::SingleLineCentered,
            });
        }
        self.frame.push(DrawOp::Text {
            text: hint.to_string(),
            font: SMALL_FONT,
            color: DIM,
            rect: Rect::new(LEFT, HINT_TOP, RIGHT, HINT_TOP + 18),
            layout: TextLayout::SingleLineCentered,
        });
        self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_stack_above_the_hint() {
        let r = button_rects(3);
        assert_eq!(r.len(), 3);
        assert!(r[0].bottom < r[1].top && r[1].bottom < r[2].top);
        assert_eq!(r[2].bottom, BUTTONS_BOTTOM);
        assert!(r[2].bottom <= HINT_TOP);
        assert!(r[0].left > PANEL.left && r[0].right < PANEL.right);
        assert_eq!(hit(&r, r[1].left, r[1].top), Some(1));
        assert_eq!(hit(&r, r[1].right, r[1].top), None);
        assert!(button_rects(0).is_empty());
    }

    #[test]
    fn overlong_paragraph_is_cut_with_an_ellipsis() {
        let font = Font::bundled();
        let mut p = Page::new(&font, "Title", 3);
        let long = "Příliš dlouhý odstavec s cestou /Users/někdo/Library/Application Support/x "
            .repeat(40);
        p.para(&long, TEXT);
        p.para("Ještě jeden.", TEXT);
        let frame = p.finish(&["A", "B", "C"], 0, "hint");
        let buttons = button_rects(3);
        let mut cut = 0;
        for op in &frame.ops {
            let DrawOp::Text {
                text,
                font: spec,
                rect,
                layout: TextLayout::WrappedLeft,
                ..
            } = op
            else {
                continue;
            };
            let laid = layout_text(&font, text, *spec, *rect, TextLayout::WrappedLeft);
            let cell = laid.scale.cell_height();
            for l in &laid.lines {
                assert!(
                    l.x + l.width <= rect.right && l.y + cell <= rect.bottom,
                    "{l:?}"
                );
            }
            assert!(rect.bottom <= buttons[0].top);
            if text.ends_with(ELLIPSIS) {
                cut += 1;
                assert!(text.contains('\n'));
            }
            assert!(
                !text.contains("Ještě"),
                "no room left for the next paragraph"
            );
        }
        assert_eq!(cut, 1);
    }
}
