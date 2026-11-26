//! CPU compositor: applies a display list to an RGBA canvas of 640s x 480s pixels.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ab_glyph::{Font as _, GlyphId, PxScale, point};

use crate::text::{Font, LaidText, layout_text};
use crate::types::{AssetId, DrawOp, Frame, Rect};
use crate::upscaled::{ContentHash, Miss, Request, RgbaFrame3x, UPSCALE, UpscaledPack};

pub const CANVAS_WIDTH: u32 = 640;
pub const CANVAS_HEIGHT: u32 = 480;

/// One palette-indexed Smacker frame, borrowed from the asset store.
#[derive(Debug, Clone, Copy)]
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
    pub palette: &'a [[u8; 3]; 256],
    /// Which frame this is, for the upscaled art; `None` draws the original pixels.
    pub ident: Option<FrameIdent<'a>>,
}

/// Identity of a source frame in the upscale pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameIdent<'a> {
    /// Normalised file name: lower case, relative to `game/`, `/` separators.
    pub file: &'a str,
    /// 1-based.
    pub frame: u32,
    /// `upscale::content_hash` of the frame's pixels and palette.
    pub hash: ContentHash,
    /// Held by a cutscene stream slot (shown once; not kept in the upscaled LRU).
    pub stream: bool,
}

/// Upscaled draws since the pack was set: hits drew pack frames, misses fell back to
/// the original pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpscaleStats {
    pub hits: u64,
    pub misses: u64,
}

/// Receives one line per file the first time a draw of it falls back.
pub type MissLog = Box<dyn FnMut(&str) + Send>;

/// Supplies decoded frames to the compositor (implemented by `engine::Assets`).
pub trait FrameSource {
    /// Frame `frame` (1-based) of `asset`, or `None` when it does not exist.
    fn smk_frame(&self, asset: AssetId, frame: u32) -> Option<FrameView<'_>>;
}

/// An RGBA8 image, rows top to bottom, alpha always 255.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl RgbaImage {
    /// A black, opaque image.
    pub fn new(width: u32, height: u32) -> Self {
        let mut data = vec![0u8; width as usize * height as usize * 4];
        for px in data.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Self {
            width,
            height,
            data,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw RGBA bytes, `width * 4` per row.
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }
}

/// Coverage bitmap of one glyph at one scale, positioned relative to the pen on the baseline.
struct GlyphBitmap {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

/// Glyph cache key: glyph id and the f32 bits of the x and y pixel scales.
type GlyphKey = (GlyphId, u32, u32);

/// Glyph cache entries kept before the cache is cleared.
const GLYPH_CACHE_LIMIT: usize = 4096;

/// Composes display lists at integer scale `scale` (1..=8).
pub struct Compositor {
    scale: u32,
    font: Font,
    canvas: RgbaImage,
    glyphs: HashMap<GlyphKey, Option<GlyphBitmap>>,
    upscaled: Option<Arc<UpscaledPack>>,
    stats: UpscaleStats,
    /// Files whose fallback was logged.
    logged: HashSet<String>,
    miss_log: Option<MissLog>,
}

impl Compositor {
    pub fn new(scale: u32, font: Font) -> Self {
        let scale = scale.clamp(1, 8);
        Self {
            scale,
            font,
            canvas: RgbaImage::new(CANVAS_WIDTH * scale, CANVAS_HEIGHT * scale),
            glyphs: HashMap::new(),
            upscaled: None,
            stats: UpscaleStats::default(),
            logged: HashSet::new(),
            miss_log: None,
        }
    }

    /// Draw Smacker frames from `pack` whenever the scale is 3 (upscaled art); `None`
    /// goes back to the original pixels. Resets the statistics and the per-file log.
    pub fn set_upscaled(&mut self, pack: Option<Arc<UpscaledPack>>) {
        self.upscaled = pack;
        self.stats = UpscaleStats::default();
        self.logged.clear();
    }

    pub fn upscaled(&self) -> Option<&Arc<UpscaledPack>> {
        self.upscaled.as_ref()
    }

    /// True when the next compose draws from the pack (a pack is set and s == 3).
    pub fn upscaled_active(&self) -> bool {
        self.upscaled.is_some() && self.scale == UPSCALE
    }

    pub fn upscale_stats(&self) -> UpscaleStats {
        self.stats
    }

    /// Where fallbacks are reported, once per file (e.g. the app's log).
    pub fn set_miss_log(&mut self, log: MissLog) {
        self.miss_log = Some(log);
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Switch to scale `scale` (clamped like [`Compositor::new`]), keeping the font. The
    /// canvas is reallocated and the glyph cache dropped when the scale changes.
    pub fn set_scale(&mut self, scale: u32) {
        let scale = scale.clamp(1, 8);
        if scale != self.scale {
            self.scale = scale;
            self.canvas = RgbaImage::new(CANVAS_WIDTH * scale, CANVAS_HEIGHT * scale);
            self.glyphs.clear();
        }
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    /// Clears the canvas to black and applies every op in order. Ops naming a missing frame
    /// are skipped.
    pub fn compose(&mut self, frame: &Frame, assets: &dyn FrameSource) -> &RgbaImage {
        self.fill(
            Rect::new(0, 0, CANVAS_WIDTH as i32, CANVAS_HEIGHT as i32),
            [0, 0, 0],
        );
        let pack = if self.upscaled_active() {
            self.upscaled.clone()
        } else {
            None
        };
        if let Some(p) = &pack {
            let reqs: Vec<Request<'_>> = frame
                .ops
                .iter()
                .filter_map(|op| match *op {
                    DrawOp::Smk {
                        asset,
                        frame,
                        transparent,
                        ..
                    } => assets
                        .smk_frame(asset, frame)
                        .and_then(|v| request(&v, transparent)),
                    _ => None,
                })
                .collect();
            p.prefetch(&reqs);
        }
        for op in &frame.ops {
            match op {
                DrawOp::Smk {
                    asset,
                    frame,
                    x,
                    y,
                    transparent,
                } => {
                    let Some(view) = assets.smk_frame(*asset, *frame) else {
                        continue;
                    };
                    if let Some(p) = &pack {
                        let got = request(&view, *transparent)
                            .ok_or(Miss::NoIdentity)
                            .and_then(|r| p.resolve(&r));
                        match got {
                            Ok(up) => {
                                self.stats.hits += 1;
                                self.blit_rgba(&up, *x, *y);
                                continue;
                            }
                            Err(m) => self.note_miss(view.ident.map(|i| i.file), m),
                        }
                    }
                    self.blit(&view, *x, *y, *transparent);
                }
                DrawOp::Fill { rect, color } => self.fill(*rect, *color),
                DrawOp::Text {
                    text,
                    font,
                    color,
                    rect,
                    layout,
                } => {
                    let laid = layout_text(&self.font, text, *font, *rect, *layout);
                    self.draw_text(&laid, *color);
                }
            }
        }
        &self.canvas
    }

    fn note_miss(&mut self, file: Option<&str>, miss: Miss) {
        self.stats.misses += 1;
        let file = file.unwrap_or("<unnamed frame>");
        if !self.logged.contains(file) {
            self.logged.insert(file.to_string());
            if let Some(log) = &mut self.miss_log {
                log(&format!(
                    "upscaled art: {file} drawn with original pixels ({})",
                    miss.describe()
                ));
            }
        }
    }

    /// Blits an upscaled frame (binary alpha) with its top-left at logical (x, y).
    fn blit_rgba(&mut self, f: &RgbaFrame3x, x: i32, y: i32) {
        let s = i64::from(self.scale);
        let (cw, ch) = (i64::from(self.canvas.width), i64::from(self.canvas.height));
        let (fw, fh) = (i64::from(f.width), i64::from(f.height));
        if f.rgba.len() != (fw * fh * 4) as usize {
            return;
        }
        let (x0, y0) = (i64::from(x) * s, i64::from(y) * s);
        let (l, t) = (x0.max(0), y0.max(0));
        let (r, b) = ((x0 + fw).min(cw), (y0 + fh).min(ch));
        if l >= r || t >= b {
            return;
        }
        let stride = self.stride();
        let n = (r - l) as usize * 4;
        for cy in t..b {
            let src_off = (((cy - y0) * fw + (l - x0)) * 4) as usize;
            let src = &f.rgba[src_off..src_off + n];
            let dst_off = cy as usize * stride + l as usize * 4;
            let dst = &mut self.canvas.data[dst_off..dst_off + n];
            if f.opaque {
                dst.copy_from_slice(src);
            } else {
                for (d, p) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0)
                {
                    if p[3] != 0 {
                        *d = *p;
                    }
                }
            }
        }
    }

    /// Canvas width in bytes per row.
    fn stride(&self) -> usize {
        self.canvas.width as usize * 4
    }

    /// Logical rect clipped to the 640x480 canvas, or `None` when empty.
    fn clip_logical(r: Rect) -> Option<(i32, i32, i32, i32)> {
        let l = r.left.max(0);
        let t = r.top.max(0);
        let rr = r.right.min(CANVAS_WIDTH as i32);
        let b = r.bottom.min(CANVAS_HEIGHT as i32);
        (l < rr && t < b).then_some((l, t, rr, b))
    }

    fn fill(&mut self, rect: Rect, color: [u8; 3]) {
        let Some((l, t, r, b)) = Self::clip_logical(rect) else {
            return;
        };
        let s = self.scale as usize;
        let stride = self.stride();
        let px = [color[0], color[1], color[2], 255];
        let (x0, x1) = (l as usize * s, r as usize * s);
        for y in t as usize * s..b as usize * s {
            let row = &mut self.canvas.data[y * stride + x0 * 4..y * stride + x1 * 4];
            for p in row.as_chunks_mut::<4>().0 {
                *p = px;
            }
        }
    }

    /// Nearest-neighbour blit of a palette-indexed frame; `transparent` skips index 0 (F6.1).
    fn blit(&mut self, view: &FrameView<'_>, x: i32, y: i32, transparent: bool) {
        let (w, h) = (view.width as i32, view.height as i32);
        if view.pixels.len() < view.width as usize * view.height as usize {
            return;
        }
        let Some((l, t, r, b)) =
            Self::clip_logical(Rect::new(x, y, x.saturating_add(w), y.saturating_add(h)))
        else {
            return;
        };
        let mut pal = [[0u8; 4]; 256];
        for (d, c) in pal.iter_mut().zip(view.palette.iter()) {
            *d = [c[0], c[1], c[2], 255];
        }
        let s = self.scale as usize;
        let stride = self.stride();
        let (dx0, dx1) = (l as usize * s * 4, r as usize * s * 4);
        for ly in t..b {
            let src_off = ((ly - y) * w + (l - x)) as usize;
            let src = &view.pixels[src_off..src_off + (r - l) as usize];
            let y0 = ly as usize * s;
            let first = y0 * stride;
            let row = &mut self.canvas.data[first + dx0..first + dx1];
            for (&idx, dst) in src.iter().zip(row.chunks_exact_mut(4 * s)) {
                if transparent && idx == 0 {
                    continue;
                }
                let c = pal[idx as usize];
                for p in dst.as_chunks_mut::<4>().0 {
                    *p = c;
                }
            }
            if transparent {
                for k in 1..s {
                    let start = (y0 + k) * stride;
                    let row = &mut self.canvas.data[start + dx0..start + dx1];
                    for (&idx, dst) in src.iter().zip(row.chunks_exact_mut(4 * s)) {
                        if idx != 0 {
                            let c = pal[idx as usize];
                            for p in dst.as_chunks_mut::<4>().0 {
                                *p = c;
                            }
                        }
                    }
                }
            } else {
                for k in 1..s {
                    let start = (y0 + k) * stride;
                    self.canvas
                        .data
                        .copy_within(first + dx0..first + dx1, start + dx0);
                }
            }
        }
    }

    /// Rasterises laid-out lines at scale s with antialiased coverage blended over the canvas
    /// (background mode TRANSPARENT), clipped to the layout's clip rect.
    fn draw_text(&mut self, laid: &LaidText, color: [u8; 3]) {
        let Some((l, t, r, b)) = Self::clip_logical(laid.clip) else {
            return;
        };
        let s = self.scale as i32;
        let (cx0, cy0, cx1, cy1) = (l * s, t * s, r * s, b * s);
        let sx = laid.scale.x * s as f32;
        let sy = laid.scale.y * s as f32;
        let height_unscaled = self.font.face.height_unscaled();
        let px_scale = PxScale {
            x: sx * height_unscaled,
            y: sy * height_unscaled,
        };
        let key_scale = (sx.to_bits(), sy.to_bits());
        if self.glyphs.len() > GLYPH_CACHE_LIMIT {
            self.glyphs.clear();
        }
        let stride = self.stride();
        for line in &laid.lines {
            if line.y >= b || line.y + laid.scale.cell_height() <= t {
                continue;
            }
            let baseline = (line.y + laid.scale.ascent) * s;
            let mut pen = line.x;
            for c in line.text.chars() {
                let id = self.font.face.glyph_id(c);
                let origin_x = pen * s;
                pen += self.font.advance(c, &laid.scale);
                let face = &self.font.face;
                let bm = self
                    .glyphs
                    .entry((id, key_scale.0, key_scale.1))
                    .or_insert_with(|| rasterise(face, id, px_scale));
                let Some(bm) = bm else { continue };
                let gx = origin_x + bm.left;
                let gy = baseline + bm.top;
                let x0 = gx.max(cx0);
                let x1 = (gx + bm.width as i32).min(cx1);
                let y0 = gy.max(cy0);
                let y1 = (gy + bm.height as i32).min(cy1);
                for py in y0..y1 {
                    let src_row = ((py - gy) as u32 * bm.width) as usize;
                    let dst_row = py as usize * stride;
                    for px in x0..x1 {
                        let a = u32::from(bm.coverage[src_row + (px - gx) as usize]);
                        if a == 0 {
                            continue;
                        }
                        let d = &mut self.canvas.data[dst_row + px as usize * 4..][..3];
                        for k in 0..3 {
                            let dv = u32::from(d[k]);
                            let cv = u32::from(color[k]);
                            d[k] = ((dv * (255 - a) + cv * a + 127) / 255) as u8;
                        }
                    }
                }
            }
        }
    }
}

/// The pack lookup for a draw of `view`, or `None` when the frame has no identity.
fn request<'a>(view: &FrameView<'a>, transparent: bool) -> Option<Request<'a>> {
    view.ident.map(|i| Request {
        file: i.file,
        frame: i.frame,
        hash: i.hash,
        src_width: view.width,
        src_height: view.height,
        transparent,
        stream: i.stream,
    })
}

/// Coverage of glyph `id` with its pen at the origin on the baseline.
fn rasterise(face: &ab_glyph::FontVec, id: GlyphId, scale: PxScale) -> Option<GlyphBitmap> {
    let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
    let outlined = face.outline_glyph(glyph)?;
    let bounds = outlined.px_bounds();
    let width = bounds.width() as u32;
    let height = bounds.height() as u32;
    let mut coverage = vec![0u8; width as usize * height as usize];
    outlined.draw(|x, y, c| {
        if x < width && y < height {
            coverage[(y * width + x) as usize] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    });
    Some(GlyphBitmap {
        left: bounds.min.x as i32,
        top: bounds.min.y as i32,
        width,
        height,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FontSpec, TextLayout};

    struct Img {
        w: u32,
        h: u32,
        px: Vec<u8>,
        pal: Box<[[u8; 3]; 256]>,
    }

    #[derive(Default)]
    struct Mock(HashMap<(u32, u32), Img>);

    impl Mock {
        fn add(&mut self, id: u32, frame: u32, w: u32, h: u32, px: Vec<u8>) {
            let mut pal = Box::new([[0u8; 3]; 256]);
            for (i, p) in pal.iter_mut().enumerate() {
                *p = [i as u8, 255 - i as u8, (i * 7) as u8];
            }
            self.0.insert((id, frame), Img { w, h, px, pal });
        }
    }

    impl FrameSource for Mock {
        fn smk_frame(&self, asset: AssetId, frame: u32) -> Option<FrameView<'_>> {
            self.0.get(&(asset.0, frame)).map(|i| FrameView {
                width: i.w,
                height: i.h,
                pixels: &i.px,
                palette: &i.pal,
                ident: None,
            })
        }
    }

    fn col(i: u8) -> [u8; 4] {
        [i, 255 - i, i.wrapping_mul(7), 255]
    }

    fn smk(id: u32, x: i32, y: i32, transparent: bool) -> DrawOp {
        DrawOp::Smk {
            asset: AssetId(id),
            frame: 1,
            x,
            y,
            transparent,
        }
    }

    fn comp(scale: u32) -> Compositor {
        Compositor::new(scale, Font::bundled())
    }

    #[test]
    fn transparent_index0_skipped() {
        let mut m = Mock::default();
        m.add(1, 1, 640, 480, vec![9; 640 * 480]);
        m.add(2, 1, 2, 2, vec![0, 5, 6, 0]);
        let mut c = comp(1);
        let f = Frame {
            ops: vec![smk(1, 0, 0, false), smk(2, 10, 20, true)],
        };
        let img = c.compose(&f, &m);
        assert_eq!(img.pixel(10, 20), col(9));
        assert_eq!(img.pixel(11, 20), col(5));
        assert_eq!(img.pixel(10, 21), col(6));
        assert_eq!(img.pixel(11, 21), col(9));
        // Opaque blit draws index 0 with its palette colour.
        let f = Frame {
            ops: vec![smk(1, 0, 0, false), smk(2, 10, 20, false)],
        };
        let img = c.compose(&f, &m);
        assert_eq!(img.pixel(10, 20), col(0));
    }

    #[test]
    fn clip_at_edges() {
        let mut m = Mock::default();
        m.add(2, 1, 4, 4, (1..=16).collect());
        let mut c = comp(2);
        let f = Frame {
            ops: vec![
                smk(2, -2, -2, false),
                smk(2, 638, 478, false),
                smk(2, -10, 100, false),
                smk(2, 700, 100, true),
            ],
        };
        let img = c.compose(&f, &m);
        assert_eq!((img.width(), img.height()), (1280, 960));
        // Top-left: source (2,2) = 11 lands at logical (0,0).
        assert_eq!(img.pixel(0, 0), col(11));
        assert_eq!(img.pixel(3, 3), col(16));
        assert_eq!(img.pixel(4, 4), [0, 0, 0, 255]);
        // Bottom-right: source (0,0) = 1 at logical (638,478), (1,1) = 6 at (639,479).
        assert_eq!(img.pixel(1276, 956), col(1));
        assert_eq!(img.pixel(1279, 959), col(6));
        // Missing frame is skipped, not a panic.
        let f = Frame {
            ops: vec![DrawOp::Smk {
                asset: AssetId(99),
                frame: 1,
                x: 0,
                y: 0,
                transparent: false,
            }],
        };
        c.compose(&f, &m);
    }

    #[test]
    fn set_scale_resizes_the_canvas() {
        let mut m = Mock::default();
        m.add(2, 1, 2, 2, vec![1, 2, 3, 4]);
        let mut c = comp(1);
        c.set_scale(3);
        assert_eq!(c.scale(), 3);
        let frame = Frame {
            ops: vec![smk(2, 1, 1, false)],
        };
        let img = c.compose(&frame, &m).clone();
        assert_eq!(img, comp(3).compose(&frame, &m).clone());
        c.set_scale(0);
        assert_eq!(c.scale(), 1);
        assert_eq!(c.compose(&frame, &m).width(), 640);
    }

    #[test]
    fn scale3_nearest() {
        let mut m = Mock::default();
        m.add(2, 1, 2, 2, vec![1, 2, 3, 4]);
        let mut c = comp(3);
        let img = c.compose(
            &Frame {
                ops: vec![smk(2, 1, 1, false)],
            },
            &m,
        );
        assert_eq!((img.width(), img.height()), (1920, 1440));
        for y in 0..12 {
            for x in 0..12 {
                let want = match ((x / 3) as i32 - 1, (y / 3) as i32 - 1) {
                    (0, 0) => col(1),
                    (1, 0) => col(2),
                    (0, 1) => col(3),
                    (1, 1) => col(4),
                    _ => [0, 0, 0, 255],
                };
                assert_eq!(img.pixel(x, y), want, "({x},{y})");
            }
        }
    }

    #[test]
    fn fill_clips_and_scales() {
        let m = Mock::default();
        let mut c = comp(2);
        let img = c.compose(
            &Frame {
                ops: vec![DrawOp::Fill {
                    rect: Rect::new(-5, 470, 3, 500),
                    color: [1, 2, 3],
                }],
            },
            &m,
        );
        assert_eq!(img.pixel(0, 940), [1, 2, 3, 255]);
        assert_eq!(img.pixel(5, 959), [1, 2, 3, 255]);
        assert_eq!(img.pixel(6, 959), [0, 0, 0, 255]);
        assert_eq!(img.pixel(0, 939), [0, 0, 0, 255]);
    }

    fn question(text: &str) -> DrawOp {
        DrawOp::Text {
            text: text.to_string(),
            font: FontSpec {
                height: 30,
                width: 15,
            },
            color: [255, 255, 255],
            rect: Rect::new(58, 82, 590, 187),
            layout: TextLayout::QuestionCentered,
        }
    }

    /// Bounding box (logical, inclusive) of every non-black pixel.
    fn ink_bounds(img: &RgbaImage, s: u32) -> Option<(u32, u32, u32, u32)> {
        let mut b: Option<(u32, u32, u32, u32)> = None;
        for y in 0..img.height() {
            for x in 0..img.width() {
                if img.pixel(x, y)[..3] != [0, 0, 0] {
                    let (lx, ly) = (x / s, y / s);
                    b = Some(match b {
                        None => (lx, ly, lx, ly),
                        Some((a, bb, c, d)) => (a.min(lx), bb.min(ly), c.max(lx), d.max(ly)),
                    });
                }
            }
        }
        b
    }

    #[test]
    fn text_stays_inside_question_rect() {
        let text = "Který z následujících českých panovníků nechal postavit Karlův most \
                    přes řeku Vltavu?";
        let m = Mock::default();
        for s in [1, 3] {
            let mut c = comp(s);
            let img = c.compose(
                &Frame {
                    ops: vec![question(text)],
                },
                &m,
            );
            let (x0, y0, x1, y1) = ink_bounds(img, s).expect("text drew something");
            assert!(
                x0 >= 58 && x1 < 590 && y0 >= 82 && y1 < 187,
                "{s}: {x0},{y0} {x1},{y1}"
            );
            // Roughly centred horizontally.
            let mid = (x0 + x1) as i32 / 2;
            assert!((mid - 324).abs() <= 12, "mid {mid}");
        }
    }

    #[test]
    fn text_renders_czech_glyphs() {
        let m = Mock::default();
        let mut c = comp(3);
        for ch in "ŘžČůě".chars() {
            let op = DrawOp::Text {
                text: ch.to_string(),
                font: FontSpec {
                    height: 20,
                    width: 10,
                },
                color: [255, 255, 255],
                rect: Rect::new(0, 0, 100, 35),
                layout: TextLayout::AnswerCentered,
            };
            let img = c.compose(&Frame { ops: vec![op] }, &m);
            let ink = img
                .as_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] != [0, 0, 0])
                .count();
            assert!(ink > 20, "{ch}: {ink} pixels");
            // The glyph id is a real glyph, not .notdef.
            use ab_glyph::Font as _;
            assert_ne!(c.font().face.glyph_id(ch).0, 0, "{ch}");
        }
    }

    #[test]
    fn text_colour_is_blended() {
        let mut m = Mock::default();
        m.add(1, 1, 640, 480, vec![200; 640 * 480]);
        let mut c = comp(2);
        let op = DrawOp::Text {
            text: "B. Ostrava".into(),
            font: FontSpec {
                height: 20,
                width: 10,
            },
            color: [0x80, 0x80, 0x80],
            rect: Rect::new(50, 267, 589, 301),
            layout: TextLayout::AnswerCentered,
        };
        let img = c.compose(
            &Frame {
                ops: vec![smk(1, 0, 0, false), op],
            },
            &m,
        );
        let bg = col(200);
        let mut full = 0;
        for p in img.as_raw().as_chunks::<4>().0 {
            if p[..3] != bg[..3] {
                // Every changed pixel lies between the background and the text colour.
                for k in 0..3 {
                    let (lo, hi) = (bg[k].min(0x80), bg[k].max(0x80));
                    assert!(p[k] >= lo && p[k] <= hi);
                }
                if p[..3] == [0x80, 0x80, 0x80] {
                    full += 1;
                }
            }
        }
        assert!(full > 50, "{full}");
    }
}
