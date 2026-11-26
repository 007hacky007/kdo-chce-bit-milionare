//! Transparency around the upscaler: which files are drawn transparent,
//! filling hidden pixels before upscaling and the binary 3x mask after.
//!
//! The original draws a frame either opaque or skipping palette index 0
//! (`analysis/findings.md` 6.1, `UsagDefn` usage 4 or 5). The choice is
//! made per load site in the engine, never per frame, and every file is
//! loaded one way only, so the rule is per file ([`blit_mode`]):
//!
//! - [`BlitMode::Opaque`] files keep index 0 as an ordinary colour (in
//!   cutscenes and backgrounds it is the black of the picture): the runner
//!   sees the true palette colours and the stored alpha is 255 everywhere.
//! - [`BlitMode::Transparent`] files have index 0 hidden: its RGB is
//!   replaced by the nearest visible pixel before upscaling, so no dark
//!   halo bleeds into the edges, and the stored alpha is the index-0 mask
//!   scaled by nearest neighbour (binary). Frames without index 0 come out
//!   fully opaque.
//!
//! The pack index records each file's mode, and the runtime
//! (`render::UpscaledPack`) uses an entry only for a draw whose
//! `transparent` flag matches it, falling back to the original pixels
//! otherwise. The engine test `blit_modes_match_upscale_rule` checks every
//! load site against [`blit_mode`].

use std::collections::VecDeque;

use crate::pack::RgbaFrame;

/// How the engine draws a file (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlitMode {
    Opaque,
    Transparent,
}

impl BlitMode {
    /// Byte stored in the pack index and the cache records.
    pub fn id(self) -> u8 {
        match self {
            BlitMode::Opaque => 0,
            BlitMode::Transparent => 1,
        }
    }

    pub fn from_id(id: u8) -> Option<BlitMode> {
        match id {
            0 => Some(BlitMode::Opaque),
            1 => Some(BlitMode::Transparent),
            _ => None,
        }
    }

    /// The mode of a draw with the engine's `transparent` flag.
    pub fn from_transparent(transparent: bool) -> BlitMode {
        if transparent {
            BlitMode::Transparent
        } else {
            BlitMode::Opaque
        }
    }
}

/// Files the engine loads opaque (`Game::load(name, false)` and
/// `cutscene::play`), as normalised [`crate::FrameKey`] names. Everything
/// under `cuts/` is a cutscene and opaque as well.
///
/// Sources: `beatup/mod.rs` (Bs_Bg), `game/mod.rs` ART_FILES (Mm_Bg,
/// Qs_Bg), `screens/mod.rs` (Opening), `screens/play_again.rs` and
/// `screens/pause.rs` (LogoStill), `screens/pick_millionaire.rs` (Pm,
/// Pm_End, Bios), `screens/player_count.rs` (Np), `screens/keys.rs`
/// (Keys2-4, BeatKey).
pub const OPAQUE_FILES: [&str; 13] = [
    "beat/bs_bg.smk",
    "menus/beatkey.smk",
    "menus/bios.smk",
    "menus/keys2.smk",
    "menus/keys3.smk",
    "menus/keys4.smk",
    "menus/logostill.smk",
    "menus/np.smk",
    "menus/opening.smk",
    "menus/pm.smk",
    "menus/pm_end.smk",
    "quesansw/mm_bg.smk",
    "quesansw/qs_bg.smk",
];

/// The blit the engine uses for `file` (a normalised key name).
pub fn blit_mode(file: &str) -> BlitMode {
    let f = file.replace('\\', "/").to_ascii_lowercase();
    if f.starts_with("cuts/") || OPAQUE_FILES.contains(&f.as_str()) {
        BlitMode::Opaque
    } else {
        BlitMode::Transparent
    }
}

/// The RGB image handed to the upscaler, `width * height * 3` bytes.
///
/// For [`BlitMode::Transparent`] every index-0 pixel takes the colour of
/// the nearest visible pixel (breadth-first over 4-neighbours, so the
/// distance is Manhattan; ties go to the earlier pixel in row order). A
/// frame with no visible pixel is black. For [`BlitMode::Opaque`] this is
/// the plain palette lookup.
pub fn fill_hidden(
    pixels: &[u8],
    palette: &[[u8; 3]; 256],
    width: u32,
    height: u32,
    mode: BlitMode,
) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let n = w * h;
    debug_assert_eq!(pixels.len(), n);
    let mut rgb = vec![0u8; n * 3];
    for (i, &p) in pixels.iter().enumerate().take(n) {
        rgb[i * 3..i * 3 + 3].copy_from_slice(&palette[p as usize]);
    }
    if mode == BlitMode::Opaque {
        return rgb;
    }
    let mut done: Vec<bool> = pixels.iter().take(n).map(|&p| p != 0).collect();
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| done[i]).collect();
    if queue.is_empty() {
        rgb.fill(0);
        return rgb;
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        let c = [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]];
        let mut visit = |j: usize| {
            if !done[j] {
                done[j] = true;
                rgb[j * 3..j * 3 + 3].copy_from_slice(&c);
                queue.push_back(j);
            }
        };
        if y > 0 {
            visit(i - w);
        }
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    rgb
}

/// Combines the upscaler's RGB output (`width * scale` x `height * scale`,
/// 3 bytes per pixel) with the source mask: alpha is 0 where the source
/// pixel under it (nearest neighbour) is index 0 and the file is
/// transparent, 255 elsewhere. The RGB is kept as the upscaler made it,
/// also under transparent pixels (see [`crate::encode`]).
///
/// Returns `None` when a buffer length does not match the dimensions.
pub fn apply_mask(
    rgb: &[u8],
    pixels: &[u8],
    width: u32,
    height: u32,
    scale: u32,
    mode: BlitMode,
) -> Option<RgbaFrame> {
    let (w, h, s) = (width as usize, height as usize, scale as usize);
    let (ow, oh) = (w.checked_mul(s)?, h.checked_mul(s)?);
    if s == 0 || pixels.len() != w * h || rgb.len() != ow * oh * 3 {
        return None;
    }
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    for y in 0..oh {
        let row = &pixels[(y / s) * w..(y / s + 1) * w];
        for x in 0..ow {
            let i = (y * ow + x) * 3;
            let hidden = mode == BlitMode::Transparent && row[x / s] == 0;
            rgba.extend_from_slice(&rgb[i..i + 3]);
            rgba.push(if hidden { 0 } else { 255 });
        }
    }
    Some(RgbaFrame {
        width: ow as u32,
        height: oh as u32,
        rgba,
    })
}
