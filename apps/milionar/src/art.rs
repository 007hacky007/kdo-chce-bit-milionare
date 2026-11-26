//! Upscaled art: opening the pack when the game starts and the F8 switch.
//!
//! The pack is `<data_dir>/upscaled/frames.upk` (made by `milionar-tool upscale`). It is
//! used only when it was made from this import (SHA-256 of `game/import.json`), by this
//! build's pipeline version, at scale 3 and with the pinned runner and model. Anything
//! else is logged once and the game runs with the original art.

use std::path::Path;
use std::sync::Arc;

use render::upscaled::{PackExpectation, UpscaledPack};
use upscale::PackError;

use crate::settings::ArtMode;

/// What F8 did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F8 {
    /// The art mode is now this (setting and screen).
    Switched(ArtMode),
    /// No valid pack: nothing changes (original art stays on screen).
    NoPack,
}

/// F8: switch between original and upscaled art, only when a valid pack is open.
pub fn f8_toggle(setting: ArtMode, pack_valid: bool) -> F8 {
    if pack_valid {
        F8::Switched(setting.other())
    } else {
        F8::NoPack
    }
}

/// The art actually drawn for a setting: upscaled needs a valid pack.
pub fn effective(setting: ArtMode, pack_valid: bool) -> ArtMode {
    if pack_valid {
        setting
    } else {
        ArtMode::Original
    }
}

/// What [`open_pack`] found in `<data_dir>/upscaled`.
#[derive(Debug)]
pub enum PackStatus {
    /// A valid pack for this import and build.
    Ready(Arc<UpscaledPack>),
    /// No pack file.
    Missing,
    /// The pack was made from a different import of the game (the import changed since).
    StaleImport,
    /// Well formed but made by another pipeline or format version, at another scale or
    /// with another runner or model; the text says which.
    Incompatible(String),
    /// Unreadable or malformed (truncated, bad magic, I/O error, no readable import
    /// manifest to check it against).
    Corrupt(String),
}

/// [`PackStatus`] without the pack, for the screens that explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    Ready,
    Missing,
    /// Made from a previous import: rebuilding is offered.
    Stale,
    /// Incompatible or corrupt.
    Unusable,
}

impl PackStatus {
    pub fn kind(&self) -> PackKind {
        match self {
            PackStatus::Ready(_) => PackKind::Ready,
            PackStatus::Missing => PackKind::Missing,
            PackStatus::StaleImport => PackKind::Stale,
            PackStatus::Incompatible(_) | PackStatus::Corrupt(_) => PackKind::Unusable,
        }
    }

    pub fn into_pack(self) -> Option<Arc<UpscaledPack>> {
        match self {
            PackStatus::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// A line for the log (for `Ready` the caller logs details itself).
    pub fn describe(&self, data_dir: &Path) -> String {
        let path = upscale::pack_path(data_dir);
        let path = path.display();
        let hint = "generate it with `milionar-tool upscale`";
        match self {
            PackStatus::Ready(p) => format!("upscaled art: {} frames in {path}", p.len()),
            PackStatus::Missing => {
                format!("upscaled art: no pack at {path} ({hint}); using the original art")
            }
            PackStatus::StaleImport => format!(
                "upscaled art: {path} was made from a different import of the game \
                 ({hint} again); using the original art"
            ),
            PackStatus::Incompatible(why) => format!(
                "upscaled art: cannot use {path}: {why} ({hint} again); using the original art"
            ),
            PackStatus::Corrupt(why) => format!(
                "upscaled art: cannot read {path}: {why} ({hint} again); using the original art"
            ),
        }
    }
}

/// Opens and validates the pack of `data_dir`.
pub fn open_pack(data_dir: &Path) -> PackStatus {
    let path = upscale::pack_path(data_dir);
    if !path.is_file() {
        return PackStatus::Missing;
    }
    let manifest = match upscale::manifest_hash(data_dir) {
        Ok(m) => m,
        Err(e) => return PackStatus::Corrupt(format!("cannot read the import manifest: {e}")),
    };
    let dir = path.parent().unwrap_or(data_dir);
    match UpscaledPack::open(dir, &manifest, &PackExpectation::pinned()) {
        Ok(p) => PackStatus::Ready(Arc::new(p)),
        Err(PackError::ManifestMismatch) => PackStatus::StaleImport,
        Err(
            e @ (PackError::UnsupportedVersion { .. }
            | PackError::PipelineMismatch { .. }
            | PackError::Incompatible(_)),
        ) => PackStatus::Incompatible(e.to_string()),
        Err(
            e @ (PackError::Io(_)
            | PackError::BadMagic
            | PackError::Truncated(_)
            | PackError::Invalid(_)),
        ) => PackStatus::Corrupt(e.to_string()),
    }
}

/// Frees the pack's decoded frames while the original art is shown (budget 0) and
/// restores the budget on switching back, so a session that turned the upscaled art off
/// does not keep hundreds of MB of RGBA it no longer draws.
#[derive(Debug, Default)]
pub struct PackBudget {
    /// The budget to restore; `Some` while the original art is shown.
    saved: Option<usize>,
}

impl PackBudget {
    pub fn apply(&mut self, pack: &UpscaledPack, art: ArtMode) {
        match art {
            ArtMode::Original => {
                if self.saved.is_none() {
                    self.saved = Some(pack.budget());
                    pack.set_budget(0);
                }
            }
            ArtMode::Upscaled => {
                if let Some(b) = self.saved.take() {
                    pack.set_budget(b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use upscale::{BlitMode, Codec, FrameKey, PackHeader, PackWriter, RgbaFrame};

    #[test]
    fn f8_toggles_only_with_valid_pack() {
        use ArtMode::*;
        assert_eq!(f8_toggle(Original, true), F8::Switched(Upscaled));
        assert_eq!(f8_toggle(Upscaled, true), F8::Switched(Original));
        assert_eq!(f8_toggle(Original, false), F8::NoPack);
        assert_eq!(f8_toggle(Upscaled, false), F8::NoPack);
        assert_eq!(effective(Upscaled, true), Upscaled);
        assert_eq!(effective(Original, true), Original);
        assert_eq!(effective(Upscaled, false), Original);
        assert_eq!(effective(Original, false), Original);
    }

    fn write_pack(data_dir: &Path, params: upscale::RunnerParams) {
        let header = PackHeader {
            params,
            source_manifest_sha256: upscale::manifest_hash(data_dir).unwrap(),
            pipeline_version: upscale::pipeline::PIPELINE_VERSION,
        };
        let path = upscale::pack_path(data_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut w = PackWriter::create(&path, &header, Codec::Raw).unwrap();
        let f = RgbaFrame {
            width: 3,
            height: 3,
            rgba: vec![255; 36],
        };
        w.add_frame(&FrameKey::new("a.smk", 1), [0; 16], BlitMode::Opaque, &f)
            .unwrap();
        w.finish().unwrap();
    }

    fn pinned_params() -> upscale::RunnerParams {
        use upscale::runner;
        let hex = |h: &str| -> [u8; 32] {
            std::array::from_fn(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
        };
        upscale::RunnerParams {
            runner_sha256: hex(runner::PINNED_RUNNER_SHA256S[0]),
            model_param_sha256: hex(runner::MODEL_PARAM_SHA256),
            model_bin_sha256: hex(runner::MODEL_BIN_SHA256),
            model_name: runner::MODEL_NAME.into(),
            scale: 3,
            tile: runner::TILE,
            tta: false,
            gpu: runner::GPU,
        }
    }

    fn imported(dir: &Path, json: &[u8]) {
        std::fs::create_dir_all(dir.join("game")).unwrap();
        std::fs::write(dir.join("game/import.json"), json).unwrap();
    }

    #[test]
    fn pack_status_typed() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        imported(d, b"{}");
        assert!(matches!(open_pack(d), PackStatus::Missing));
        // A pack made with the pinned runner from this import is used.
        write_pack(d, pinned_params());
        let PackStatus::Ready(p) = open_pack(d) else {
            panic!("{:?}", open_pack(d));
        };
        assert_eq!(p.len(), 1);
        // A re-import changes the manifest: the pack is stale.
        imported(d, b"{\"again\": 1}");
        let st = open_pack(d);
        assert!(matches!(st, PackStatus::StaleImport), "{st:?}");
        assert!(
            st.describe(d).contains("different import"),
            "{}",
            st.describe(d)
        );
        // A pack from the test stand-in runner is not used by the game.
        write_pack(d, upscale::FakeRunner::fake_params());
        let st = open_pack(d);
        assert!(matches!(st, PackStatus::Incompatible(_)), "{st:?}");
        assert!(st.describe(d).contains("pinned"), "{}", st.describe(d));
        // Garbage in place of the pack.
        std::fs::write(upscale::pack_path(d), b"not a pack at all").unwrap();
        let st = open_pack(d);
        assert!(matches!(st, PackStatus::Corrupt(_)), "{st:?}");
        // Every status but Ready logs a line that says the original art is used.
        for st in [
            PackStatus::Missing,
            PackStatus::StaleImport,
            PackStatus::Incompatible("x".into()),
            PackStatus::Corrupt("y".into()),
        ] {
            assert!(
                st.describe(d).contains("original art"),
                "{}",
                st.describe(d)
            );
            assert!(st.into_pack().is_none());
        }
    }

    /// The rebuild sequence: the app closes the pack (the screen drops its only handle),
    /// the pipeline renames the new pack into place, the game opens it again. The rename
    /// also succeeds while a handle is open on macOS and Linux (the old handle keeps
    /// reading the old file); Windows refuses to replace an open file, which is why the
    /// app closes it first.
    #[test]
    fn pack_replaced_while_open() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        imported(d, b"{}");
        let frame = |v: u8| RgbaFrame {
            width: 3,
            height: 3,
            rgba: [v, v, v, 255].repeat(9),
        };
        let write = |v: u8| {
            let header = PackHeader {
                params: pinned_params(),
                source_manifest_sha256: upscale::manifest_hash(d).unwrap(),
                pipeline_version: upscale::pipeline::PIPELINE_VERSION,
            };
            let path = upscale::pack_path(d);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut w = PackWriter::create(&path, &header, Codec::Raw).unwrap();
            w.add_frame(
                &FrameKey::new("a.smk", 1),
                [0; 16],
                BlitMode::Opaque,
                &frame(v),
            )
            .unwrap();
            w.finish().unwrap();
        };
        let req = render::upscaled::Request {
            file: "a.smk",
            frame: 1,
            hash: [0; 16],
            src_width: 1,
            src_height: 1,
            transparent: false,
            stream: false,
        };
        let first_byte = |p: &UpscaledPack| p.resolve(&req).unwrap().rgba[0];
        write(10);
        let open = open_pack(d).into_pack().unwrap();
        assert_eq!(first_byte(&open), 10);
        // The app's order: close (the last handle goes), promote, reopen.
        assert_eq!(Arc::strong_count(&open), 1);
        drop(open);
        write(20);
        let reopened = open_pack(d).into_pack().unwrap();
        assert_eq!(first_byte(&reopened), 20);
        #[cfg(unix)]
        {
            // A real rename over the open file: the open handle keeps the old frames.
            reopened.set_budget(0);
            write(30);
            assert_eq!(first_byte(&reopened), 20);
            drop(reopened);
            assert_eq!(first_byte(&open_pack(d).into_pack().unwrap()), 30);
        }
    }

    #[test]
    fn original_art_frees_the_lru() {
        let dir = tempfile::tempdir().unwrap();
        imported(dir.path(), b"{}");
        write_pack(dir.path(), pinned_params());
        let pack = open_pack(dir.path()).into_pack().unwrap();
        let req = render::upscaled::Request {
            file: "a.smk",
            frame: 1,
            hash: [0; 16],
            src_width: 1,
            src_height: 1,
            transparent: false,
            stream: false,
        };
        pack.resolve(&req).unwrap();
        assert_eq!(pack.memory_bytes(), 36);
        let budget = pack.budget();
        let mut b = PackBudget::default();
        b.apply(&pack, ArtMode::Original);
        assert_eq!((pack.memory_bytes(), pack.budget()), (0, 0));
        // Repeated switches keep the real budget.
        b.apply(&pack, ArtMode::Original);
        b.apply(&pack, ArtMode::Upscaled);
        assert_eq!(pack.budget(), budget);
        b.apply(&pack, ArtMode::Upscaled);
        assert_eq!(pack.budget(), budget);
        pack.resolve(&req).unwrap();
        assert_eq!(pack.memory_bytes(), 36);
    }
}
