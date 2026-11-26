//! Display list types and the CPU compositor. No game logic lives here.
//!
//! [`Compositor`] turns a [`Frame`] into an RGBA canvas of 640s x 480s pixels: Smacker art is
//! upscaled by nearest neighbour (or, at s = 3 with an [`UpscaledPack`], drawn from the
//! AI-upscaled pack), text is rasterised at scale s with GDI-style metrics (F8).
#![forbid(unsafe_code)]

pub mod compose;
pub mod text;
pub mod types;
pub mod upscaled;

pub use compose::{
    CANVAS_HEIGHT, CANVAS_WIDTH, Compositor, FrameIdent, FrameSource, FrameView, MissLog,
    RgbaImage, UpscaleStats,
};
pub use text::{
    BUNDLED_FONT, Font, FontError, FontMetrics, FontSource, GdiScale, LaidLine, LaidText,
    gdi_scale, layout_text, load_font, load_font_from, system_font_candidates,
};
pub use types::{AssetId, DrawOp, FontSpec, Frame, Rect, TextLayout};
pub use upscaled::{PackExpectation, RgbaFrame3x, UpscaledPack};
