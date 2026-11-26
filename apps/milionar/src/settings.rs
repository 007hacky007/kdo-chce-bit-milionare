//! `<data_dir>/settings.json`: scaling mode, fullscreen, window size, volume, art mode.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How the 640x480 picture fills the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScalingMode {
    /// Largest aspect-correct size, letterboxed (default).
    #[default]
    Fit,
    /// Largest integer multiple of 640x480, centred.
    Integer,
    /// Fill the whole window, ignoring the aspect ratio.
    Stretch,
}

/// Which art the compositor draws (F8 switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtMode {
    /// The original 640x480 pixels, scaled to the window.
    Original,
    /// AI-upscaled frames from `<data_dir>/upscaled/frames.upk` (default). Used only while
    /// a valid pack is present; otherwise the game runs in original mode and the setting
    /// is kept, so the art comes back once the pack has been generated.
    #[default]
    Upscaled,
}

impl ArtMode {
    pub fn other(self) -> ArtMode {
        match self {
            ArtMode::Original => ArtMode::Upscaled,
            ArtMode::Upscaled => ArtMode::Original,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ArtMode::Original => "original",
            ArtMode::Upscaled => "upscaled",
        }
    }
}

/// Written with serde; read leniently by [`Settings::load`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub scaling: ScalingMode,
    pub fullscreen: bool,
    /// Windowed size in window coordinates (points on macOS).
    pub window_width: u32,
    pub window_height: u32,
    /// Master volume 0..=100.
    pub volume: u32,
    pub art_mode: ArtMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scaling: ScalingMode::Fit,
            fullscreen: false,
            window_width: 960,
            window_height: 720,
            volume: 100,
            art_mode: ArtMode::Upscaled,
        }
    }
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

impl Settings {
    /// Reads the settings; a missing file gives the defaults, a damaged one the defaults
    /// plus a warning on stderr (never an error: settings must not stop the game).
    pub fn load(path: &Path) -> Settings {
        let text = match std::fs::read(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Settings::default(),
            Err(e) => {
                eprintln!(
                    "warning: cannot read {}: {e}; using defaults",
                    path.display()
                );
                return Settings::default();
            }
        };
        match serde_json::from_slice::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(map)) => Settings::from_fields(&map, path),
            Ok(_) => {
                eprintln!("warning: ignoring {}: not a JSON object", path.display());
                Settings::default()
            }
            Err(e) => {
                eprintln!("warning: ignoring damaged {}: {e}", path.display());
                Settings::default()
            }
        }
    }

    /// Field by field, so one bad or unknown value does not discard the others. Numbers
    /// are clamped to their ranges; a value of the wrong type keeps that field's default.
    fn from_fields(map: &serde_json::Map<String, serde_json::Value>, path: &Path) -> Settings {
        let mut s = Settings::default();
        let warn = |key: &str| eprintln!("warning: {}: ignoring bad \"{key}\"", path.display());
        let num = |key: &str, lo: f64, hi: f64| -> Option<u32> {
            let v = map.get(key)?;
            match v.as_f64() {
                Some(n) if n.is_finite() => Some(n.clamp(lo, hi) as u32),
                _ => {
                    warn(key);
                    None
                }
            }
        };
        if let Some(v) = map.get("scaling") {
            match serde_json::from_value::<ScalingMode>(v.clone()) {
                Ok(m) => s.scaling = m,
                Err(_) => warn("scaling"),
            }
        }
        if let Some(v) = map.get("fullscreen") {
            match v.as_bool() {
                Some(b) => s.fullscreen = b,
                None => warn("fullscreen"),
            }
        }
        if let Some(v) = map.get("art_mode") {
            match serde_json::from_value::<ArtMode>(v.clone()) {
                Ok(m) => s.art_mode = m,
                Err(_) => warn("art_mode"),
            }
        }
        if let Some(w) = num("window_width", 320.0, 16384.0) {
            s.window_width = w;
        }
        if let Some(h) = num("window_height", 240.0, 16384.0) {
            s.window_height = h;
        }
        if let Some(v) = num("volume", 0.0, 100.0) {
            s.volume = v;
        }
        s
    }

    /// Writes atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings {
            scaling: ScalingMode::Integer,
            fullscreen: true,
            window_width: 1234,
            window_height: 777,
            volume: 42,
            art_mode: ArtMode::Upscaled,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"integer\""), "{text}");
    }

    #[test]
    fn settings_art_mode_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        assert_eq!(Settings::default().art_mode, ArtMode::Upscaled);
        for mode in [ArtMode::Original, ArtMode::Upscaled] {
            let s = Settings {
                art_mode: mode,
                ..Settings::default()
            };
            s.save(&path).unwrap();
            assert_eq!(Settings::load(&path), s);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains(&format!("\"art_mode\": \"{}\"", mode.name())),
                "{text}"
            );
        }
        // Tolerant: a bad value keeps the default and the other fields; a file from
        // before the setting existed gets the default.
        std::fs::write(&path, br#"{"art_mode":"sepia","volume":7}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!((s.art_mode, s.volume), (ArtMode::Upscaled, 7));
        std::fs::write(&path, br#"{"art_mode":3}"#).unwrap();
        assert_eq!(Settings::load(&path).art_mode, ArtMode::Upscaled);
        std::fs::write(&path, br#"{"art_mode":"original"}"#).unwrap();
        assert_eq!(Settings::load(&path).art_mode, ArtMode::Original);
        std::fs::write(&path, br#"{"volume":50}"#).unwrap();
        assert_eq!(Settings::load(&path).art_mode, ArtMode::Upscaled);
    }

    #[test]
    fn bad_fields_do_not_discard_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        let load = |text: &str| {
            std::fs::write(&path, text).unwrap();
            Settings::load(&path)
        };
        let s = load(r#"{"scaling":"integer","volume":1000,"future_option":[1,2]}"#);
        assert_eq!((s.scaling, s.volume), (ScalingMode::Integer, 100));
        let s = load(r#"{"fullscreen":true,"volume":-5,"scaling":"bogus","window_width":"x"}"#);
        assert!(s.fullscreen);
        assert_eq!(s.volume, 0);
        assert_eq!(s.scaling, ScalingMode::Fit);
        assert_eq!(s.window_width, Settings::default().window_width);
        let s = load(r#"{"window_width":99999999999,"window_height":800,"volume":55.7}"#);
        assert_eq!(s.window_height, 800);
        assert_eq!(s.window_width, 16384);
        assert_eq!(s.volume, 55);
        assert_eq!(load("[1,2]"), Settings::default());
    }

    #[test]
    fn corrupt_settings_json_uses_defaults_and_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        Settings::default().save(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap())
            .expect("rewritten file is valid JSON");
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, br#"{"scaling":"stretch","volume":250}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.scaling, ScalingMode::Stretch);
        assert_eq!(s.volume, 100, "out-of-range volume is clamped");
        assert_eq!(s.window_width, Settings::default().window_width);
        std::fs::write(&path, br#"{"window_width":0,"window_height":5}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(
            (s.window_width, s.window_height),
            (320, 240),
            "tiny windows are raised to a usable minimum"
        );
    }
}
