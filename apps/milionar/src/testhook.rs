//! Testing aids for unattended runs (cargo feature `test-hooks`, off in normal builds).
//!
//! - `MILIONAR_SCRIPT="ms:action;..."` pushes SDL events at the given milliseconds after
//!   start, so they go through the same event handling as real input. Actions:
//!   `key:<SDL key name>` (down + up, e.g. `key:Space`, `key:1`, `key:Escape`),
//!   `down:<name>` / `up:<name>`, `click:x,y` / `rclick:x,y` (window coordinates),
//!   `fs` (fullscreen toggle as F11 would), `size:WxH` (resize the window), `quit`.
//! - `MILIONAR_INJECT=<file>`: polled while running; when the file exists its contents
//!   (same syntax, times relative to now) are added to the script and the file is
//!   removed. Lets a driver react to what the captures show.
//! - `MILIONAR_PICK=<path>`: the setup wizard's "choose" buttons take this path instead
//!   of opening a file dialog (dialogs cannot be scripted); read in `setup_ui`.
//! - `MILIONAR_LANG=cs|en`: the language of the port's own screens instead of the
//!   system's; read in `strings`.
//! - `MILIONAR_CAPTURE_DIR=<dir>` writes the window back buffer as PNG every
//!   `MILIONAR_CAPTURE_MS` (default 1000) milliseconds, and logs the mixer state with it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sdl3::event::Event;
use sdl3::keyboard::{Keycode, Mod, Scancode};
use sdl3::mouse::MouseButton;
use sdl3::pixels::PixelFormat;
use sdl3::render::WindowCanvas;

#[derive(Debug, Clone)]
enum Action {
    Down(String),
    Up(String),
    Click(f32, f32, MouseButton),
    Fullscreen,
    Size(u32, u32),
    Quit,
}

pub struct TestHook {
    start: Instant,
    script: Vec<(u64, Action)>,
    next: usize,
    capture: Option<(PathBuf, Duration, Option<Instant>)>,
    inject: Option<PathBuf>,
}

/// What the main loop must do itself (needs `&mut Settings`).
pub enum Request {
    ToggleFullscreen,
}

fn parse(script: &str) -> Vec<(u64, Action)> {
    let mut out = Vec::new();
    for item in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((t, act)) = item.split_once(':') else {
            eprintln!("testhook: bad item {item:?}");
            continue;
        };
        let Ok(t) = t.parse::<u64>() else {
            eprintln!("testhook: bad time {item:?}");
            continue;
        };
        let (verb, arg) = act.split_once(':').unwrap_or((act, ""));
        let xy = |s: &str| -> Option<(f32, f32)> {
            let (x, y) = s.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        };
        match verb {
            "key" => {
                out.push((t, Action::Down(arg.to_string())));
                out.push((t + 30, Action::Up(arg.to_string())));
            }
            "down" => out.push((t, Action::Down(arg.to_string()))),
            "up" => out.push((t, Action::Up(arg.to_string()))),
            "click" | "rclick" => match xy(arg) {
                Some((x, y)) => {
                    let b = if verb == "click" {
                        MouseButton::Left
                    } else {
                        MouseButton::Right
                    };
                    out.push((t, Action::Click(x, y, b)));
                }
                None => eprintln!("testhook: bad click {item:?}"),
            },
            "fs" => out.push((t, Action::Fullscreen)),
            "size" => match arg
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            {
                Some((w, h)) => out.push((t, Action::Size(w, h))),
                None => eprintln!("testhook: bad size {item:?}"),
            },
            "quit" => out.push((t, Action::Quit)),
            _ => eprintln!("testhook: unknown action {item:?}"),
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

fn key_event(name: &str, down: bool) -> Option<Event> {
    let scancode = Scancode::from_name(name);
    let keycode = Keycode::from_name(name);
    if scancode.is_none() && keycode.is_none() {
        eprintln!("testhook: unknown key {name:?}");
        return None;
    }
    Some(if down {
        Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode,
            scancode,
            keymod: Mod::NOMOD,
            repeat: false,
            which: 0,
            raw: 0,
        }
    } else {
        Event::KeyUp {
            timestamp: 0,
            window_id: 0,
            keycode,
            scancode,
            keymod: Mod::NOMOD,
            repeat: false,
            which: 0,
            raw: 0,
        }
    })
}

impl TestHook {
    pub fn from_env() -> Option<TestHook> {
        let script = std::env::var("MILIONAR_SCRIPT").ok();
        let dir = std::env::var_os("MILIONAR_CAPTURE_DIR").map(PathBuf::from);
        let inject = std::env::var_os("MILIONAR_INJECT").map(PathBuf::from);
        if script.is_none() && dir.is_none() && inject.is_none() {
            return None;
        }
        let every = std::env::var("MILIONAR_CAPTURE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        if let Some(d) = &dir {
            let _ = std::fs::create_dir_all(d);
        }
        Some(TestHook {
            start: Instant::now(),
            script: script.as_deref().map(parse).unwrap_or_default(),
            next: 0,
            capture: dir.map(|d| (d, Duration::from_millis(every), None)),
            inject,
        })
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Push the script events that are due.
    pub fn tick(
        &mut self,
        events: &sdl3::EventSubsystem,
        canvas: &mut WindowCanvas,
    ) -> Vec<Request> {
        let now = self.elapsed_ms();
        if let Some(path) = &self.inject
            && let Ok(text) = std::fs::read_to_string(path)
        {
            let _ = std::fs::remove_file(path);
            let mut rest = self.script.split_off(self.next);
            rest.extend(parse(&text).into_iter().map(|(t, a)| (t + now, a)));
            rest.sort_by_key(|(t, _)| *t);
            self.script.extend(rest);
        }
        let mut requests = Vec::new();
        while let Some((t, a)) = self.script.get(self.next).cloned() {
            if t > now {
                break;
            }
            self.next += 1;
            eprintln!("testhook: {now} ms: {a:?}");
            let ev = match a {
                Action::Down(k) => key_event(&k, true),
                Action::Up(k) => key_event(&k, false),
                Action::Click(x, y, b) => Some(Event::MouseButtonDown {
                    timestamp: 0,
                    window_id: 0,
                    which: 0,
                    mouse_btn: b,
                    clicks: 1,
                    x,
                    y,
                }),
                Action::Fullscreen => {
                    requests.push(Request::ToggleFullscreen);
                    None
                }
                Action::Size(w, h) => {
                    let _ = canvas.window_mut().set_size(w, h);
                    None
                }
                Action::Quit => Some(Event::Quit { timestamp: 0 }),
            };
            if let Some(ev) = ev
                && let Err(e) = events.push_event(ev)
            {
                eprintln!("testhook: push failed: {e}");
            }
        }
        requests
    }

    /// Called after drawing, before `present`: save the back buffer when due.
    pub fn after_draw(&mut self, canvas: &WindowCanvas, status: impl FnOnce() -> String) {
        let Some((dir, every, last)) = &mut self.capture else {
            return;
        };
        if last.is_some_and(|l| l.elapsed() < *every) {
            return;
        }
        *last = Some(Instant::now());
        let ms = self.start.elapsed().as_millis();
        let path = dir.join(format!("t{ms:06}.png"));
        match save_png(canvas, &path) {
            Ok((w, h)) => eprintln!("capture: {} {w}x{h} {}", path.display(), status()),
            Err(e) => eprintln!("capture failed: {e}"),
        }
    }
}

fn save_png(canvas: &WindowCanvas, path: &std::path::Path) -> Result<(u32, u32), String> {
    let surface = canvas.read_pixels(None).map_err(|e| e.to_string())?;
    let surface = surface
        .convert_format(PixelFormat::RGBA32)
        .map_err(|e| e.to_string())?;
    let (w, h, pitch) = (surface.width(), surface.height(), surface.pitch() as usize);
    let mut data = Vec::with_capacity(w as usize * h as usize * 4);
    surface.with_lock(|px| {
        for y in 0..h as usize {
            data.extend_from_slice(&px[y * pitch..y * pitch + w as usize * 4]);
        }
    });
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&data))
        .map_err(|e| e.to_string())?;
    Ok((w, h))
}
