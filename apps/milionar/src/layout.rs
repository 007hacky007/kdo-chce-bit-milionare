//! Pure geometry of the window: compositor scale, where the 640x480 picture lands in the
//! window, and the inverse mapping of mouse positions. All sizes are physical pixels.

use crate::settings::ScalingMode;

pub const GAME_W: u32 = 640;
pub const GAME_H: u32 = 480;

/// Destination rectangle of the picture inside the window, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dest {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Compositor scale s = clamp(floor(min(w/640, h/480)), 1, 4).
pub fn scale_choice(win_w: u32, win_h: u32) -> u32 {
    (win_w / GAME_W).min(win_h / GAME_H).clamp(1, 4)
}

/// Where the picture goes for a window of `win_w` x `win_h` physical pixels.
pub fn dest_rect(mode: ScalingMode, win_w: u32, win_h: u32) -> Dest {
    let (w, h) = match mode {
        ScalingMode::Stretch => (win_w, win_h),
        ScalingMode::Integer if win_w >= GAME_W && win_h >= GAME_H => {
            let k = (win_w / GAME_W).min(win_h / GAME_H);
            (GAME_W * k, GAME_H * k)
        }
        // Fit (and Integer in a window smaller than 640x480). Integer arithmetic so the
        // limiting side fills the window exactly.
        _ => {
            let (ww, wh) = (u64::from(win_w), u64::from(win_h));
            if ww * u64::from(GAME_H) <= wh * u64::from(GAME_W) {
                (win_w, (ww * u64::from(GAME_H) / u64::from(GAME_W)) as u32)
            } else {
                ((wh * u64::from(GAME_W) / u64::from(GAME_H)) as u32, win_h)
            }
        }
    };
    Dest {
        x: ((win_w - w) / 2) as i32,
        y: ((win_h - h) / 2) as i32,
        w,
        h,
    }
}

/// Window position (physical pixels) to 640x480 game coordinates, clamped to the picture.
pub fn map_mouse(dest: Dest, x: f32, y: f32) -> (i32, i32) {
    fn axis(p: f32, origin: i32, size: u32, game: u32) -> i32 {
        if size == 0 {
            return 0;
        }
        let v = ((f64::from(p) - f64::from(origin)) * f64::from(game) / f64::from(size)).floor();
        v.clamp(0.0, f64::from(game - 1)) as i32
    }
    (
        axis(x, dest.x, dest.w, GAME_W),
        axis(y, dest.y, dest.h, GAME_H),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_mapping_fit_mode() {
        let d = dest_rect(ScalingMode::Fit, 1920, 1080);
        assert_eq!(
            d,
            Dest {
                x: 240,
                y: 0,
                w: 1440,
                h: 1080
            }
        );
        assert_eq!(map_mouse(d, 240.0 + 720.0, 540.0), (320, 240));
        assert_eq!(map_mouse(d, 240.0, 0.0), (0, 0));
        // Letterbox bars clamp to the nearest edge of the picture.
        assert_eq!(map_mouse(d, 10.0, 540.0), (0, 240));
        assert_eq!(map_mouse(d, 1915.0, 1079.9), (639, 479));
        assert_eq!(map_mouse(d, -50.0, -50.0), (0, 0));
        // Pillarbox the other way round.
        let d = dest_rect(ScalingMode::Fit, 1280, 1200);
        assert_eq!(
            d,
            Dest {
                x: 0,
                y: 120,
                w: 1280,
                h: 960
            }
        );
        assert_eq!(map_mouse(d, 640.0, 600.0), (320, 240));
        assert_eq!(map_mouse(d, 640.0, 50.0), (320, 0));
    }

    #[test]
    fn hidpi_window_maps_like_its_pixel_size() {
        // A 1024x768-point window at density 2 is 2048x1536 pixels; the host scales event
        // positions by the density before mapping.
        let d = dest_rect(ScalingMode::Fit, 2048, 1536);
        assert_eq!(map_mouse(d, 512.0 * 2.0, 384.0 * 2.0), (320, 240));
        assert_eq!(super::scale_choice(2048, 1536), 3);
    }

    #[test]
    fn integer_and_stretch_modes() {
        let d = dest_rect(ScalingMode::Integer, 1920, 1080);
        assert_eq!(
            d,
            Dest {
                x: 320,
                y: 60,
                w: 1280,
                h: 960
            }
        );
        assert_eq!(map_mouse(d, 320.0 + 640.0, 60.0 + 480.0), (320, 240));
        // Smaller than one integer step: falls back to fit.
        assert_eq!(
            dest_rect(ScalingMode::Integer, 320, 240),
            dest_rect(ScalingMode::Fit, 320, 240)
        );
        let d = dest_rect(ScalingMode::Stretch, 1920, 1080);
        assert_eq!(
            d,
            Dest {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
        assert_eq!(map_mouse(d, 960.0, 540.0), (320, 240));
        // A zero-sized (minimised) window does not divide by zero.
        let d = dest_rect(ScalingMode::Fit, 0, 0);
        assert_eq!(map_mouse(d, 5.0, 5.0), (0, 0));
    }

    #[test]
    fn scale_choice() {
        assert_eq!(super::scale_choice(1280, 960), 2);
        assert_eq!(super::scale_choice(1920, 1440), 3);
        assert_eq!(super::scale_choice(800, 600), 1);
        assert_eq!(super::scale_choice(100, 100), 1);
        assert_eq!(super::scale_choice(5120, 2880), 4);
        assert_eq!(super::scale_choice(1920, 1080), 2);
    }
}
