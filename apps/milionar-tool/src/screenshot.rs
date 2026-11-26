//! `screenshot`: run a replay script on the headless host and save the frames presented
//! at given ticks as PNG (local inspection; the output contains game art, keep it out of
//! the repository).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use engine::replay::{self, Script};
use engine::{
    Assets, AudioCmd, DebugState, Frame, Game, HeadlessHost, Host, InputState, Pacing, Quit,
    SharedFrames, SilentSource, SoundId, SoundSource,
};
use gamedata::GameData;
use render::{Compositor, RgbaImage, load_font};

pub struct Options {
    pub script: PathBuf,
    pub seed: Option<u32>,
    pub ticks: Vec<u64>,
    pub out: PathBuf,
    pub scale: u32,
    pub silent: bool,
}

/// Wraps the headless host and composes the frame of every wanted present before it
/// returns (Smk references are valid only during `present`). Quits after the last one.
struct Capture {
    inner: HeadlessHost,
    shared: SharedFrames,
    comp: Compositor,
    ticks: Vec<u64>,
    images: Vec<(u64, RgbaImage)>,
}

impl Host for Capture {
    fn present(&mut self, frame: Frame, pacing: Pacing) -> Result<InputState, Quit> {
        let index = self.inner.ticks();
        if self.ticks.contains(&index) {
            let snap = self.shared.snapshot(&frame);
            let img = self.comp.compose(&frame, &snap).clone();
            self.images.push((index, img));
        }
        if self.ticks.iter().all(|&t| t <= index) {
            return Err(Quit);
        }
        self.inner.present(frame, pacing)
    }
    fn audio(&mut self, cmd: AudioCmd) {
        self.inner.audio(cmd);
    }
    fn voice_busy(&self) -> bool {
        self.inner.voice_busy()
    }
    fn voice_envelope(&self) -> u8 {
        self.inner.voice_envelope()
    }
    fn sound_playing(&self, id: SoundId) -> bool {
        self.inner.sound_playing(id)
    }
    fn flush_input(&mut self) {
        self.inner.flush_input();
    }
    fn pump_voice(&mut self) {
        self.inner.pump_voice();
    }
    fn now_ms(&self) -> u64 {
        self.inner.now_ms()
    }
    fn observe_debug(&mut self, state: &DebugState) {
        self.inner.observe_debug(state);
    }
}

/// Output path for `tick`: `out` itself for a single tick, else `{tick}` in `out`
/// replaced by the tick.
fn out_path(out: &Path, tick: u64, many: bool) -> PathBuf {
    let s = out.to_string_lossy();
    if s.contains("{tick}") {
        PathBuf::from(s.replace("{tick}", &format!("{tick:06}")))
    } else if many {
        let stem = out.file_stem().unwrap_or_default().to_string_lossy();
        out.with_file_name(format!("{stem}_{tick:06}.png"))
    } else {
        out.to_path_buf()
    }
}

fn write_png(path: &Path, img: &RgbaImage) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(img.as_raw()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub struct Outcome {
    pub written: Vec<PathBuf>,
    /// Screen entries (present index, screen), the last 256.
    pub log: Vec<(u64, String)>,
    /// Presents the run made.
    pub presents: u64,
    /// Requested ticks the run never reached.
    pub missing: Vec<u64>,
}

/// Runs the script and writes one PNG per reached tick.
pub fn run(data_dir: &Path, o: &Options) -> Result<Outcome, String> {
    let text =
        std::fs::read_to_string(&o.script).map_err(|e| format!("{}: {e}", o.script.display()))?;
    let mut script = Script::parse(&text).map_err(|e| format!("{}: {e}", o.script.display()))?;
    if let Some(seed) = o.seed {
        script.seed = seed;
    }
    if o.ticks.is_empty() {
        return Err("no --tick given".into());
    }
    let open = || GameData::open(data_dir).map_err(|e| e.to_string());
    let mut assets = Assets::new(open()?);
    let shared = assets.share_frames();
    // Saves go to a temporary directory so a capture never moves the real question order.
    let saves = std::env::temp_dir().join(format!("milionar-screenshot-{}", std::process::id()));
    let mut game = Game::new(assets).with_saves_dir(&saves);
    let source: Box<dyn SoundSource + Send> = if o.silent {
        Box::new(SilentSource)
    } else {
        Box::new(Assets::new(open()?))
    };
    let (inner, _driver) = replay::host(&script, source);
    let mut host = Capture {
        inner,
        shared,
        comp: Compositor::new(o.scale, load_font()),
        ticks: o.ticks.clone(),
        images: Vec::new(),
    };
    let log = {
        let mut ctx = engine::Ctx::new(&mut host, script.seed);
        let _ = engine::run(&mut ctx, &mut game);
        ctx.debug.screen_log.clone()
    };
    let _ = std::fs::remove_dir_all(&saves);
    let many = o.ticks.len() > 1;
    let mut written = Vec::new();
    for (tick, img) in &host.images {
        let path = out_path(&o.out, *tick, many);
        write_png(&path, img)?;
        written.push(path);
    }
    let missing = o
        .ticks
        .iter()
        .copied()
        .filter(|t| !host.images.iter().any(|(i, _)| i == t))
        .collect();
    Ok(Outcome {
        written,
        log: log.into_iter().map(|(t, s)| (t, s.to_string())).collect(),
        presents: host.inner.ticks(),
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_paths() {
        let p = Path::new("/x/shot.png");
        assert_eq!(out_path(p, 5, false), PathBuf::from("/x/shot.png"));
        assert_eq!(out_path(p, 5, true), PathBuf::from("/x/shot_000005.png"));
        assert_eq!(
            out_path(Path::new("/x/s{tick}.png"), 42, true),
            PathBuf::from("/x/s000042.png")
        );
    }
}
