//! GDI-style text: font loading, `CreateFontA` metrics and `DrawText` layout (F8).
//!
//! The original creates `CreateFontA(height, width, ..., "Arial")` around every draw and calls
//! `DrawTextA` on a TRANSPARENT background. This module reproduces the GDI metrics and the
//! three layouts in logical 640x480 units; the compositor rasterises the laid-out lines at
//! scale s.

use std::path::{Path, PathBuf};

use crate::types::{FontSpec, Rect, TextLayout};

/// Liberation Sans Regular 2.1.5 (SIL OFL 1.1), metric-compatible with Arial.
pub const BUNDLED_FONT: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");

/// Where a [`Font`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontSource {
    System(PathBuf),
    Bundled,
}

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("cannot read font {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid font data ({origin})")]
    Invalid { origin: String },
}

/// The font-unit metrics GDI uses for `CreateFontA`.
#[derive(Debug, Clone, PartialEq)]
pub struct FontMetrics {
    pub units_per_em: u16,
    /// OS/2 usWinAscent (hhea ascender when OS/2 is absent).
    pub win_ascent: u16,
    /// OS/2 usWinDescent as a positive number.
    pub win_descent: u16,
    /// Average character width in font units, see [`gdi_scale`].
    pub avg_char_width: f32,
    /// The VDMX table's 1:1 group: (ppem, yMax, yMin) in pixels of the hinted font, by
    /// ascending ppem. Empty when the font has none ([`ARIAL_VDMX`] is used instead).
    pub vdmx: Vec<(u16, i16, i16)>,
}

/// Arial's VDMX 1:1 records for ppem 8..=48 (ppem, yMax, yMin), used for fonts without a
/// VDMX table (the bundled Liberation Sans is metric-compatible with Arial, so GDI's
/// choice for Arial is the one to copy).
pub const ARIAL_VDMX: [(u16, i16, i16); 41] = [
    (8, 8, -2),
    (9, 9, -3),
    (10, 10, -3),
    (11, 11, -3),
    (12, 12, -3),
    (13, 13, -3),
    (14, 13, -3),
    (15, 14, -3),
    (16, 15, -3),
    (17, 15, -4),
    (18, 17, -4),
    (19, 18, -4),
    (20, 19, -4),
    (21, 19, -5),
    (22, 20, -5),
    (23, 21, -5),
    (24, 21, -6),
    (25, 23, -5),
    (26, 25, -6),
    (27, 26, -6),
    (28, 26, -6),
    (29, 27, -6),
    (30, 28, -7),
    (31, 28, -7),
    (32, 29, -7),
    (33, 31, -7),
    (34, 32, -7),
    (35, 32, -8),
    (36, 33, -8),
    (37, 34, -8),
    (38, 34, -9),
    (39, 35, -9),
    (40, 36, -9),
    (41, 38, -9),
    (42, 38, -9),
    (43, 39, -10),
    (44, 40, -10),
    (45, 40, -10),
    (46, 42, -10),
    (47, 43, -10),
    (48, 45, -10),
];

/// The 1:1 (or any-aspect) group of a VDMX table: (ppem, yMax, yMin) records.
fn read_vdmx(data: &[u8]) -> Vec<(u16, i16, i16)> {
    let Some(t) = ttf_parser::RawFace::parse(data, 0)
        .ok()
        .and_then(|f| f.table(ttf_parser::Tag::from_bytes(b"VDMX")))
    else {
        return Vec::new();
    };
    let u16_at = |o: usize| t.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let (Some(_version), Some(_recs), Some(ratios)) = (u16_at(0), u16_at(2), u16_at(4)) else {
        return Vec::new();
    };
    let ratios = usize::from(ratios);
    // Ratio records (bCharSet, xRatio, yStartRatio, yEndRatio); 1:1 matches the screen,
    // 0:0:0 matches any aspect.
    let group = (0..ratios).find(|&i| {
        t.get(6 + 4 * i..10 + 4 * i).is_some_and(|r| {
            let (x, y0, y1) = (r[1], r[2], r[3]);
            (x, y0, y1) == (0, 0, 0) || (x != 0 && y0 <= x && x <= y1)
        })
    });
    let Some(group) = group else {
        return Vec::new();
    };
    let Some(off) = u16_at(6 + 4 * ratios + 2 * group).map(usize::from) else {
        return Vec::new();
    };
    let Some(n) = u16_at(off) else {
        return Vec::new();
    };
    (0..usize::from(n))
        .map_while(|i| {
            let o = off + 4 + 6 * i;
            let b = t.get(o..o + 6)?;
            Some((
                u16::from_be_bytes([b[0], b[1]]),
                i16::from_be_bytes([b[2], b[3]]),
                i16::from_be_bytes([b[4], b[5]]),
            ))
        })
        .collect()
}

/// A loaded TrueType face plus its GDI metrics.
pub struct Font {
    pub(crate) face: ab_glyph::FontVec,
    metrics: FontMetrics,
    source: FontSource,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("metrics", &self.metrics)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl Font {
    pub fn from_bytes(data: Vec<u8>, source: FontSource) -> Result<Self, FontError> {
        let origin = || match &source {
            FontSource::System(p) => p.display().to_string(),
            FontSource::Bundled => "bundled".to_string(),
        };
        let metrics = read_metrics(&data).ok_or_else(|| FontError::Invalid { origin: origin() })?;
        let face = ab_glyph::FontVec::try_from_vec(data)
            .map_err(|_| FontError::Invalid { origin: origin() })?;
        Ok(Self {
            face,
            metrics,
            source,
        })
    }

    pub fn from_path(path: &Path) -> Result<Self, FontError> {
        let data = std::fs::read(path).map_err(|source| FontError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_bytes(data, FontSource::System(path.to_path_buf()))
    }

    /// The vendored Liberation Sans.
    pub fn bundled() -> Self {
        Self::from_bytes(BUNDLED_FONT.to_vec(), FontSource::Bundled)
            .expect("the bundled font is valid")
    }

    pub fn metrics(&self) -> &FontMetrics {
        &self.metrics
    }

    pub fn source(&self) -> &FontSource {
        &self.source
    }

    /// Advance of `c` in logical pixels, rounded per glyph like GDI's integer widths.
    pub(crate) fn advance(&self, c: char, scale: &GdiScale) -> i32 {
        use ab_glyph::Font as _;
        let id = self.face.glyph_id(c);
        (self.face.h_advance_unscaled(id) * scale.x).round() as i32
    }
}

/// Letter weights (per 1000) of the OS/2 version 0-2 `xAvgCharWidth` definition: lowercase
/// a-z and space.
const AVG_WEIGHTS: [(char, u32); 27] = [
    ('a', 64),
    ('b', 14),
    ('c', 27),
    ('d', 35),
    ('e', 100),
    ('f', 20),
    ('g', 14),
    ('h', 42),
    ('i', 63),
    ('j', 3),
    ('k', 6),
    ('l', 35),
    ('m', 20),
    ('n', 56),
    ('o', 56),
    ('p', 17),
    ('q', 4),
    ('r', 49),
    ('s', 56),
    ('t', 71),
    ('u', 31),
    ('v', 10),
    ('w', 18),
    ('x', 3),
    ('y', 18),
    ('z', 2),
    (' ', 166),
];

fn read_metrics(data: &[u8]) -> Option<FontMetrics> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let units_per_em = face.units_per_em();
    let (asc, desc) = match face.tables().os2 {
        Some(os2) => (os2.windows_ascender(), -os2.windows_descender()),
        None => (face.ascender(), -face.descender()),
    };
    if asc <= 0 || desc < 0 || units_per_em == 0 {
        return None;
    }
    let stored = face
        .raw_face()
        .table(ttf_parser::Tag::from_bytes(b"OS/2"))
        .and_then(|t| t.get(2..4))
        .map(|b| i16::from_be_bytes([b[0], b[1]]))
        .unwrap_or(0);
    let weighted: Option<u32> = AVG_WEIGHTS.iter().try_fold(0u32, |acc, &(c, w)| {
        let adv = face.glyph_hor_advance(face.glyph_index(c)?)?;
        Some(acc + u32::from(adv) * w)
    });
    let avg_char_width = match weighted {
        Some(sum) if sum > 0 => sum as f32 / 1000.0,
        _ if stored > 0 => f32::from(stored),
        _ => f32::from(units_per_em) / 2.0,
    };
    Some(FontMetrics {
        units_per_em,
        win_ascent: asc as u16,
        win_descent: desc as u16,
        avg_char_width,
        vdmx: read_vdmx(data),
    })
}

/// System Arial locations for the current platform, in search order.
pub fn system_font_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.push(PathBuf::from(
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ));
        v.push(PathBuf::from("/Library/Fonts/Arial.ttf"));
    }
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        v.push(PathBuf::from(windir).join("Fonts").join("arial.ttf"));
    }
    v
}

/// System Arial, else the bundled Liberation Sans.
pub fn load_font() -> Font {
    load_font_from(&system_font_candidates())
}

/// The first candidate that loads, else the bundled font.
pub fn load_font_from(candidates: &[PathBuf]) -> Font {
    candidates
        .iter()
        .find_map(|p| Font::from_path(p).ok())
        .unwrap_or_else(Font::bundled)
}

/// Pixels per font unit for one `CreateFontA(height, width)` at scale 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GdiScale {
    pub x: f32,
    pub y: f32,
    /// tmAscent in logical pixels.
    pub ascent: i32,
    /// tmDescent in logical pixels.
    pub descent: i32,
}

impl GdiScale {
    /// tmHeight: the line height `DrawText` uses (no external leading).
    pub fn cell_height(&self) -> i32 {
        self.ascent + self.descent
    }
}

/// GDI `CreateFontA` scaling, in logical pixels per font unit.
///
/// - `height > 0` is the cell height. GDI realises the largest ppem whose hinted cell (the
///   VDMX record's yMax - yMin) fits in `height`, and the font's tmAscent / tmDescent are
///   that record's yMax / -yMin, so the cell can be smaller than asked: Arial 30 becomes
///   ppem 25 with a 23 + 5 = 28 px cell, Arial 20 ppem 17 with 15 + 4 = 19 (matches the
///   reference captures of the original, F8). `y = ppem / unitsPerEm`. Without VDMX data
///   for the height, the unhinted cell: `y = height / (usWinAscent + usWinDescent)`,
///   tmAscent = round(usWinAscent * y), tmDescent = height - tmAscent.
/// - `height < 0` is the em height: `y = -height / unitsPerEm` (unused by the game).
/// - `height == 0` uses GDI's default 12 px cell (unused by the game).
/// - `width > 0` is the average character width tmAveCharWidth, which GDI takes from OS/2
///   `xAvgCharWidth` at the requested (unhinted) size: `x = width / avg_char_width`, then
///   scaled like y by the realised ppem over the requested em (this rescale is inferred
///   from the reference captures, not from GDI documentation). `avg_char_width` is the
///   OS/2 version 0-2 weighted average of the a-z and space advances; for Arial it is
///   904.46, the stored xAvgCharWidth being its floor (904), and the metric-compatible
///   Liberation Sans gives the same value (its stored field, 1187, follows the newer
///   all-glyph rule GDI did not use for Arial).
/// - `width <= 0` keeps the natural aspect: `x = y`.
///
/// For the question font 30 x 15 with Arial: y = 25 / 2048, x = 15 / 904.46 * 25 / 26.85,
/// so glyphs are 1.27 times wider than their natural shape.
pub fn gdi_scale(m: &FontMetrics, spec: FontSpec) -> GdiScale {
    let cell_units = f32::from(m.win_ascent) + f32::from(m.win_descent);
    let height = if spec.height == 0 { 12 } else { spec.height };
    let upem = f32::from(m.units_per_em);
    let natural_x = |y: f32| {
        if spec.width > 0 {
            spec.width as f32 / m.avg_char_width
        } else {
            y
        }
    };
    if height > 0 {
        let table: &[(u16, i16, i16)] = if m.vdmx.is_empty() {
            &ARIAL_VDMX
        } else {
            &m.vdmx
        };
        let fits = table
            .iter()
            .filter(|&&(_, y_max, y_min)| {
                y_max > 0 && i32::from(y_max) - i32::from(y_min) <= height
            })
            .max_by_key(|r| r.0);
        // Only when a larger ppem in the table does not fit (else the height is beyond
        // the table and the unhinted cell is used).
        let beyond = table
            .iter()
            .all(|&(_, y_max, y_min)| i32::from(y_max) - i32::from(y_min) <= height);
        if let (Some(&(ppem, y_max, y_min)), false) = (fits, beyond) {
            let requested_y = height as f32 / cell_units;
            let y = f32::from(ppem) / upem;
            return GdiScale {
                x: natural_x(requested_y) * (y / requested_y),
                y,
                ascent: i32::from(y_max),
                descent: -i32::from(y_min),
            };
        }
        let y = height as f32 / cell_units;
        let ascent = (f32::from(m.win_ascent) * y).round() as i32;
        return GdiScale {
            x: natural_x(y),
            y,
            ascent,
            descent: height - ascent,
        };
    }
    let y = (-height) as f32 / upem;
    GdiScale {
        x: natural_x(y),
        y,
        ascent: (f32::from(m.win_ascent) * y).round() as i32,
        descent: (f32::from(m.win_descent) * y).round() as i32,
    }
}

/// One laid-out line in logical coordinates: pen origin x, cell top y.
#[derive(Debug, Clone, PartialEq)]
pub struct LaidLine {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
}

/// The result of a `DrawText` layout.
#[derive(Debug, Clone, PartialEq)]
pub struct LaidText {
    pub lines: Vec<LaidLine>,
    /// The rectangle `DrawText` clips to.
    pub clip: Rect,
    pub scale: GdiScale,
}

/// Lays out `text` exactly as the original's `DrawTextA` calls do for `layout` (F8).
///
/// - Question (0x407c80): `h = DrawText(DT_CALCRECT|DT_WORDBREAK)`, `top = rect.top +
///   (106 - h) / 2`, then `DT_CENTER|DT_WORDBREAK` in `(left, top, right, top + h)`.
/// - Answer (0x407d50): `h = DrawText(DT_CALCRECT)` without DT_WORDBREAK (one cell per hard
///   line), `top = rect.top + (35 - h) / 2`, then `DT_CENTER|DT_WORDBREAK` in
///   `(left, top, right, top + h)`, so a wrapped second line is clipped as in the original.
/// - Timer (0x410190): `DT_CENTER|DT_VCENTER|DT_SINGLELINE` in `rect`.
///
/// Divisions truncate toward zero like C. DT_CENTER puts a line at `(left + right - w) / 2`
/// and DT_VCENTER at `top + (bottom - top) / 2 - h / 2`.
pub fn layout_text(
    font: &Font,
    text: &str,
    spec: FontSpec,
    rect: Rect,
    layout: TextLayout,
) -> LaidText {
    let scale = gdi_scale(font.metrics(), spec);
    let cell = scale.cell_height();
    let place = |rows: Vec<(String, i32)>, top: i32| -> Vec<LaidLine> {
        rows.into_iter()
            .enumerate()
            .map(|(i, (text, width))| LaidLine {
                x: (rect.left + rect.right - width) / 2,
                y: top + cell * i as i32,
                text,
                width,
            })
            .collect()
    };
    match layout {
        TextLayout::QuestionCentered | TextLayout::AnswerCentered => {
            let rows = wrap(font, &scale, text, rect.width());
            let (h, span) = if layout == TextLayout::QuestionCentered {
                (cell * rows.len() as i32, 106)
            } else {
                (cell * hard_lines(text), 35)
            };
            let top = rect.top + (span - h) / 2;
            LaidText {
                lines: place(rows, top),
                clip: Rect::new(rect.left, top, rect.right, top + h),
                scale,
            }
        }
        TextLayout::WrappedLeft => LaidText {
            lines: wrap(font, &scale, text, rect.width())
                .into_iter()
                .enumerate()
                .map(|(i, (text, width))| LaidLine {
                    x: rect.left,
                    y: rect.top + cell * i as i32,
                    text,
                    width,
                })
                .collect(),
            clip: rect,
            scale,
        },
        TextLayout::SingleLineCentered => {
            let line: String = text.chars().filter(|&c| c != '\r' && c != '\n').collect();
            let width = line.chars().map(|c| font.advance(c, &scale)).sum();
            let top = rect.top + rect.height() / 2 - cell / 2;
            LaidText {
                lines: place(vec![(line, width)], top),
                clip: rect,
                scale,
            }
        }
    }
}

/// Lines `DT_CALCRECT` counts without DT_WORDBREAK: one per hard line, none for "".
fn hard_lines(text: &str) -> i32 {
    if text.is_empty() {
        0
    } else {
        text.split('\n').count() as i32
    }
}

/// DT_WORDBREAK: greedy breaks at spaces; a word wider than the line breaks at a character
/// boundary; spaces at a break are dropped; `\n` forces a break. Returns (text, width) per
/// line, the width excluding trailing spaces.
fn wrap(font: &Font, scale: &GdiScale, text: &str, max_w: i32) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    if text.is_empty() {
        return out;
    }
    for para in text.split('\n') {
        let chars: Vec<(char, i32)> = para
            .chars()
            .filter(|&c| c != '\r')
            .map(|c| (c, font.advance(c, scale)))
            .collect();
        let n = chars.len();
        let width_of = |a: usize, b: usize| -> i32 {
            let mut e = b;
            while e > a && chars[e - 1].0 == ' ' {
                e -= 1;
            }
            chars[a..e].iter().map(|&(_, w)| w).sum()
        };
        let emit = |out: &mut Vec<(String, i32)>, a: usize, b: usize| {
            let mut e = b;
            while e > a && chars[e - 1].0 == ' ' {
                e -= 1;
            }
            out.push((
                chars[a..e].iter().map(|&(c, _)| c).collect(),
                width_of(a, e),
            ));
        };
        if n == 0 {
            out.push((String::new(), 0));
            continue;
        }
        let mut i = 0;
        while i < n {
            let mut w = 0;
            let mut last_break: Option<usize> = None;
            let mut end = n;
            let mut j = i;
            while j < n {
                let (c, cw) = chars[j];
                if c == ' ' {
                    if j > i && chars[j - 1].0 != ' ' {
                        last_break = Some(j);
                    }
                    w += cw;
                    j += 1;
                    continue;
                }
                if w + cw > max_w {
                    end = match last_break {
                        Some(b) => b,
                        None => j.max(i + 1),
                    };
                    break;
                }
                w += cw;
                j += 1;
            }
            emit(&mut out, i, end);
            i = end;
            if i < n {
                while i < n && chars[i].0 == ' ' {
                    i += 1;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARIAL_MAC: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";
    const QUESTION_RECT: Rect = Rect::new(58, 82, 590, 187);
    const QUESTION_FONT: FontSpec = FontSpec {
        height: 30,
        width: 15,
    };

    fn arial() -> Option<Font> {
        Font::from_path(Path::new(ARIAL_MAC)).ok()
    }

    #[test]
    fn arial_metrics_match_os2() {
        // Arial's OS/2 table: usWinAscent 1854, usWinDescent 434, xAvgCharWidth 904, 2048 upem.
        let Some(f) = arial() else {
            eprintln!("system Arial not present; skipping");
            return;
        };
        let m = f.metrics();
        assert_eq!(
            (m.units_per_em, m.win_ascent, m.win_descent),
            (2048, 1854, 434)
        );
        assert_eq!(m.avg_char_width.floor(), 904.0);
    }

    #[test]
    fn bundled_metrics_match_arial() {
        let m = Font::bundled().metrics().clone();
        assert_eq!(
            (m.units_per_em, m.win_ascent, m.win_descent),
            (2048, 1854, 434)
        );
        // The stored xAvgCharWidth of Liberation Sans is 1187 (all-glyph average); the GDI
        // weighted formula over its Arial-compatible advances gives Arial's 904.
        assert_eq!(m.avg_char_width.floor(), 904.0);
    }

    fn arial_like(vdmx: Vec<(u16, i16, i16)>) -> FontMetrics {
        FontMetrics {
            units_per_em: 2048,
            win_ascent: 1854,
            win_descent: 434,
            avg_char_width: 904.0,
            vdmx,
        }
    }

    #[test]
    fn gdi_scale_formula() {
        // A cell beyond the VDMX table: the unhinted cell.
        let m = arial_like(vec![(8, 8, -2)]);
        let q = gdi_scale(&m, QUESTION_FONT);
        assert!((q.y - 30.0 / 2288.0).abs() < 1e-7);
        assert!((q.x - 15.0 / 904.0).abs() < 1e-7);
        assert_eq!((q.ascent, q.descent, q.cell_height()), (24, 6, 30));
        // width 0 keeps the natural aspect.
        let n = gdi_scale(
            &m,
            FontSpec {
                height: 20,
                width: 0,
            },
        );
        assert_eq!(n.x, n.y);
    }

    /// The reference captures of the original (Windows GDI, Arial) show a 28 px line
    /// pitch for the 30 x 15 question font, 18 px capitals and about 0.0156 px per
    /// horizontal font unit; and 12 px capitals, 0.0104 px per unit for the 20 x 10
    /// answers. GDI realises the largest ppem whose VDMX cell fits (25 and 17).
    #[test]
    fn positive_height_uses_the_vdmx_cell() {
        for m in [arial_like(Vec::new()), arial_like(ARIAL_VDMX.to_vec())] {
            let q = gdi_scale(&m, QUESTION_FONT);
            assert_eq!((q.ascent, q.descent, q.cell_height()), (23, 5, 28));
            assert!((q.y - 25.0 / 2048.0).abs() < 1e-7);
            let cap = 1467.0 * q.y;
            assert_eq!(cap.round(), 18.0);
            assert!((q.x - 0.01544).abs() < 0.0001, "{}", q.x);
            let a = gdi_scale(
                &m,
                FontSpec {
                    height: 20,
                    width: 10,
                },
            );
            assert_eq!((a.ascent, a.descent), (15, 4));
            assert!((a.y - 17.0 / 2048.0).abs() < 1e-7);
            assert!((a.x - 0.0105).abs() < 0.0001, "{}", a.x);
        }
        if let Some(f) = arial() {
            assert_eq!(&f.metrics().vdmx[9..10], &[(17, 15, -4)]);
            assert_eq!(gdi_scale(f.metrics(), QUESTION_FONT).cell_height(), 28);
        }
    }

    const LONG_CZECH: &str = "Který z následujících českých panovníků nechal postavit Karlův most \
                              přes řeku Vltavu?";

    #[test]
    fn text_wraps_like_drawtext() {
        let font = Font::bundled();
        let t = layout_text(
            &font,
            LONG_CZECH,
            QUESTION_FONT,
            QUESTION_RECT,
            TextLayout::QuestionCentered,
        );
        assert_eq!(t.lines.len(), 3, "{:#?}", t.lines);
        // Reference greedy word wrap over the same advances.
        let words: Vec<&str> = LONG_CZECH.split(' ').collect();
        let mut expected: Vec<String> = Vec::new();
        let mut cur = String::new();
        for w in words {
            let cand = if cur.is_empty() {
                w.to_string()
            } else {
                format!("{cur} {w}")
            };
            if measure(&font, QUESTION_FONT, &cand) <= QUESTION_RECT.width() || cur.is_empty() {
                cur = cand;
            } else {
                expected.push(std::mem::take(&mut cur));
                cur = w.to_string();
            }
        }
        expected.push(cur);
        let got: Vec<&str> = t.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(got, expected);
        // Arial 30 realises a 28 px cell (VDMX, see gdi_scale).
        let h = 3 * 28;
        let top = 82 + (106 - h) / 2;
        assert_eq!(t.clip, Rect::new(58, top, 590, top + h));
        for (i, l) in t.lines.iter().enumerate() {
            assert!(l.width <= 532);
            assert_eq!(l.x, (58 + 590 - l.width) / 2, "line {i} centred");
            assert_eq!(l.y, top + 28 * i as i32);
        }
    }

    #[test]
    fn arial_and_bundled_lay_out_identically() {
        // Liberation Sans shares Arial's advances, so the fallback keeps the line breaks.
        let Some(a) = arial() else { return };
        let b = Font::bundled();
        for (spec, layout, rect) in [
            (QUESTION_FONT, TextLayout::QuestionCentered, QUESTION_RECT),
            (
                FontSpec {
                    height: 20,
                    width: 10,
                },
                TextLayout::AnswerCentered,
                Rect::new(50, 217, 589, 251),
            ),
        ] {
            assert_eq!(
                layout_text(&a, LONG_CZECH, spec, rect, layout).lines,
                layout_text(&b, LONG_CZECH, spec, rect, layout).lines
            );
        }
    }

    #[test]
    fn four_lines_overflow_like_original() {
        // 4 x 28 = 112 px of text: top = 82 + (106 - 112) / 2 = 79, not clamped.
        let text = format!("{LONG_CZECH} A kdy založil Univerzitu Karlovu?");
        let t = layout_text(
            &Font::bundled(),
            &text,
            QUESTION_FONT,
            QUESTION_RECT,
            TextLayout::QuestionCentered,
        );
        assert_eq!(t.lines.len(), 4);
        assert_eq!(t.clip, Rect::new(58, 79, 590, 191));
    }

    fn measure(font: &Font, spec: FontSpec, s: &str) -> i32 {
        let t = layout_text(
            font,
            s,
            spec,
            Rect::new(0, 0, 100_000, 100),
            TextLayout::SingleLineCentered,
        );
        t.lines[0].width
    }

    #[test]
    fn overlong_word_breaks_by_character() {
        let font = Font::bundled();
        let word = "Nejneobhospodařovávatelnějšímu".repeat(3);
        let t = layout_text(
            &font,
            &word,
            QUESTION_FONT,
            QUESTION_RECT,
            TextLayout::QuestionCentered,
        );
        assert!(t.lines.len() >= 2);
        let joined: String = t.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(joined, word);
        assert!(t.lines.iter().all(|l| l.width <= 532));
    }

    #[test]
    fn answer_height_ignores_wrapping() {
        // DT_CALCRECT without DT_WORDBREAK (0x400): one line of height, a wrapped second line
        // falls outside the clip rect exactly as in the original.
        let font = Font::bundled();
        let spec = FontSpec {
            height: 20,
            width: 10,
        };
        let rect = Rect::new(50, 217, 589, 251);
        let long = "A. ".to_string() + &"slovo ".repeat(30);
        let t = layout_text(&font, &long, spec, rect, TextLayout::AnswerCentered);
        assert!(t.lines.len() >= 2);
        // Arial 20 realises a 19 px cell.
        let top = 217 + (35 - 19) / 2;
        assert_eq!(t.clip, Rect::new(50, top, 589, top + 19));
    }

    #[test]
    fn timer_single_line_vcentred() {
        let font = Font::bundled();
        let spec = FontSpec {
            height: 20,
            width: 15,
        };
        // FUN_00410190: SetRect(x, y, x - 1 + 32, y - 1 + 24).
        let rect = Rect::new(4, 172, 35, 195);
        let t = layout_text(&font, "12", spec, rect, TextLayout::SingleLineCentered);
        assert_eq!(t.lines.len(), 1);
        assert_eq!(t.lines[0].y, 172 + 23 / 2 - 19 / 2);
        assert_eq!(t.lines[0].x, (4 + 35 - t.lines[0].width) / 2);
        assert_eq!(t.clip, rect);
    }

    #[test]
    fn font_fallback_when_arial_missing() {
        let dir = std::env::temp_dir().join("milionar-render-font-test");
        std::fs::create_dir_all(&dir).unwrap();
        let corrupt = dir.join("corrupt.ttf");
        std::fs::write(&corrupt, b"not a font").unwrap();
        let f = load_font_from(&[PathBuf::from("/nonexistent/arial.ttf"), corrupt]);
        assert_eq!(f.source(), &FontSource::Bundled);
        if Path::new(ARIAL_MAC).exists() {
            let f = load_font_from(&[PathBuf::from(ARIAL_MAC)]);
            assert_eq!(f.source(), &FontSource::System(PathBuf::from(ARIAL_MAC)));
        }
    }

    #[test]
    fn wrapped_left_layout() {
        let font = Font::bundled();
        let spec = FontSpec {
            height: 20,
            width: 0,
        };
        let rect = Rect::new(50, 100, 250, 300);
        let t = layout_text(
            &font,
            "Příliš žluťoučký kůň úpěl ďábelské ódy\nnový řádek",
            spec,
            rect,
            TextLayout::WrappedLeft,
        );
        assert!(t.lines.len() >= 3, "{:?}", t.lines);
        let cell = t.scale.cell_height();
        for (i, l) in t.lines.iter().enumerate() {
            assert_eq!(l.x, 50);
            assert_eq!(l.y, 100 + cell * i as i32);
            assert!(l.width <= rect.width(), "{l:?}");
        }
        assert_eq!(t.lines.last().unwrap().text, "nový řádek");
        assert_eq!(t.clip, rect);
    }
}
