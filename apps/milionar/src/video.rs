//! Main-thread presentation: compose the latest frame at scale s, upload it to a
//! streaming texture and draw it letterboxed into the window.
//!
//! Original art picks s from the window ([`layout::scale_choice`]). Upscaled art pins
//! s = 3 (1920x1440, the pack's size) whatever the window, then the same letterbox
//! scaling. Texture filtering: nearest when the picture lands at exactly 640s x 480s or
//! (original art only) in integer scaling mode; linear otherwise, which in upscaled art
//! also covers integer mode (a 1920x1440 texture into 640k x 480k is not an integer
//! ratio unless k = 3, and linear averages the downscale to smaller windows).

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::FrameSnapshot;
use render::upscaled::{UPSCALE, UpscaledPack};
use render::{Compositor, Font, Frame};
use sdl3::pixels::{Color, PixelFormat};
use sdl3::render::{FRect, ScaleMode, Texture, TextureCreator, WindowCanvas};
use sdl3::video::WindowContext;

use crate::layout::{self, Dest, GAME_H, GAME_W};
use crate::settings::{ArtMode, ScalingMode};

/// Compose times since the last [`Screen::take_compose_stats`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ComposeStats {
    pub count: u32,
    pub total: Duration,
    pub max: Duration,
}

impl ComposeStats {
    fn add(&mut self, d: Duration) {
        self.count += 1;
        self.total += d;
        self.max = self.max.max(d);
    }

    pub fn describe(&self) -> String {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let mean = if self.count > 0 {
            ms(self.total) / f64::from(self.count)
        } else {
            0.0
        };
        format!(
            "{} composes, mean {mean:.2} ms, max {:.2} ms",
            self.count,
            ms(self.max)
        )
    }
}

pub struct Screen<'t> {
    creator: &'t TextureCreator<WindowContext>,
    texture: Option<Texture<'t>>,
    compositor: Compositor,
    mode: ScalingMode,
    /// The validated upscaled pack, if any.
    pack: Option<Arc<UpscaledPack>>,
    /// Frees the pack's decoded frames while the original art is shown.
    budget: crate::art::PackBudget,
    art: ArtMode,
    stats: ComposeStats,
    last_compose: Duration,
    /// The frame on screen; recomposed when the scale changes.
    current: Option<(Frame, FrameSnapshot)>,
    /// Drawn over `current` (the options overlay).
    overlay: Option<Frame>,
    dirty: bool,
    dest: Dest,
}

impl<'t> Screen<'t> {
    pub fn new(creator: &'t TextureCreator<WindowContext>, font: Font, mode: ScalingMode) -> Self {
        let mut compositor = Compositor::new(1, font);
        compositor.set_miss_log(Box::new(crate::log::log));
        Self {
            creator,
            texture: None,
            compositor,
            mode,
            pack: None,
            budget: crate::art::PackBudget::default(),
            art: ArtMode::Original,
            stats: ComposeStats::default(),
            last_compose: Duration::ZERO,
            current: None,
            overlay: None,
            dirty: true,
            dest: Dest {
                x: 0,
                y: 0,
                w: GAME_W,
                h: GAME_H,
            },
        }
    }

    /// Draw `art` from the next compose on (a frame boundary); upscaled needs the pack
    /// and falls back to original without one.
    pub fn set_art(&mut self, art: ArtMode) {
        let art = crate::art::effective(art, self.pack.is_some());
        if art == self.art {
            return;
        }
        self.art = art;
        self.compositor.set_upscaled(match art {
            ArtMode::Upscaled => self.pack.clone(),
            ArtMode::Original => None,
        });
        if let Some(p) = &self.pack {
            self.budget.apply(p, art);
        }
        self.dirty = true;
    }

    /// Replaces the upscaled pack (a game start, later a re-import or rebuild). The
    /// screen falls back to the original art; the caller sets the art mode again.
    pub fn set_pack(&mut self, pack: Option<Arc<UpscaledPack>>) {
        self.compositor.set_upscaled(None);
        self.pack = pack;
        self.budget = crate::art::PackBudget::default();
        self.art = ArtMode::Original;
        self.dirty = true;
    }

    /// The font text is drawn with (for laying out the setup screens).
    pub fn font(&self) -> &Font {
        self.compositor.font()
    }

    pub fn art(&self) -> ArtMode {
        self.art
    }

    /// Compose times since the last call (for the log).
    pub fn take_compose_stats(&mut self) -> ComposeStats {
        std::mem::take(&mut self.stats)
    }

    /// Duration of the latest compose.
    #[cfg(feature = "test-hooks")]
    pub fn last_compose(&self) -> Duration {
        self.last_compose
    }

    /// A valid upscaled pack is open (F8 can switch).
    pub fn has_pack(&self) -> bool {
        self.pack.is_some()
    }

    /// The upscaled pack's decoded bytes (LRU, stream frames), if a pack is open.
    pub fn pack_memory(&self) -> Option<(usize, usize)> {
        self.pack
            .as_ref()
            .map(|p| (p.memory_bytes(), p.stream_bytes()))
    }

    /// Draws `overlay` over the frame on screen (the frozen game frame or the wizard
    /// page) until it is cleared with `None`.
    pub fn set_overlay(&mut self, overlay: Option<Frame>) {
        self.overlay = overlay;
        self.dirty = true;
    }

    /// How the picture fills the window, from the next draw on.
    pub fn set_scaling(&mut self, mode: ScalingMode) {
        self.mode = mode;
        self.dirty = true;
    }

    pub fn set_frame(&mut self, frame: Frame, snapshot: FrameSnapshot) {
        self.current = Some((frame, snapshot));
        self.dirty = true;
    }

    /// Where the picture was last drawn (physical pixels), for mapping the mouse.
    pub fn dest(&self) -> Dest {
        self.dest
    }

    /// Recompose if needed and redraw the back buffer; the caller presents.
    pub fn draw(&mut self, canvas: &mut WindowCanvas) -> Result<(), String> {
        let (pw, ph) = canvas.output_size().map_err(|e| e.to_string())?;
        if pw == 0 || ph == 0 {
            return Ok(());
        }
        let upscaled = self.art == ArtMode::Upscaled;
        let s = if upscaled {
            UPSCALE
        } else {
            layout::scale_choice(pw, ph)
        };
        if s != self.compositor.scale() || self.texture.is_none() {
            self.compositor.set_scale(s);
            let mut t = self
                .creator
                .create_texture_streaming(PixelFormat::RGBA32, GAME_W * s, GAME_H * s)
                .map_err(|e| e.to_string())?;
            t.set_scale_mode(ScaleMode::Linear);
            self.texture = Some(t);
            self.dirty = true;
        }
        let texture = self.texture.as_mut().expect("created above");
        if self.dirty {
            if let Some((frame, snap)) = &self.current {
                let t0 = Instant::now();
                let with_overlay;
                let frame = match &self.overlay {
                    Some(o) => {
                        let mut f = frame.clone();
                        f.ops.extend(o.ops.iter().cloned());
                        with_overlay = f;
                        &with_overlay
                    }
                    None => frame,
                };
                let img = self.compositor.compose(frame, snap);
                self.last_compose = t0.elapsed();
                self.stats.add(self.last_compose);
                texture
                    .update(None, img.as_raw(), img.width() as usize * 4)
                    .map_err(|e| e.to_string())?;
            }
            self.dirty = false;
        }
        self.dest = layout::dest_rect(self.mode, pw, ph);
        // Exact size (integer mode at s, or a window of exactly 640s x 480s): no filtering.
        let exact = self.dest.w == GAME_W * s && self.dest.h == GAME_H * s;
        texture.set_scale_mode(
            if exact || (!upscaled && self.mode == ScalingMode::Integer) {
                ScaleMode::Nearest
            } else {
                ScaleMode::Linear
            },
        );
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        if self.current.is_some() {
            let d = self.dest;
            canvas
                .copy(
                    texture,
                    None,
                    FRect::new(d.x as f32, d.y as f32, d.w as f32, d.h as f32),
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
