//! AI-upscaled art mode: frame keys, the upscale pack format, the runner
//! and the pipeline that builds the pack.
//!
//! The pack layout and pipeline rules are documented in `docs/upscale-pack.md`.
//!
//! Unsafe code is denied; the only exception is the Windows Job Object FFI
//! in `runner::job`.

#![deny(unsafe_code)]

use std::path::{Path, PathBuf};

pub mod alpha;
pub mod cache;
pub mod img;
pub mod key;
pub mod pack;
pub mod pipeline;
pub mod runner;

pub use alpha::{BlitMode, blit_mode};
pub use cache::{EncodedFrame, FrameCache};
pub use key::{ContentHash, FrameKey, content_hash};
pub use pack::{
    Codec, Entry, FORMAT_VERSION, JPEG_QUALITY, PackError, PackHeader, PackReader, PackWriter,
    RgbaFrame, RunnerParams, decode, encode,
};
pub use pipeline::{Options, Phase, Progress, Report};
pub use runner::{FakeRunner, GpuProbe, Runner, UpscaleError, Upscaler};

/// File name of the pack inside `<data_dir>/upscaled/`.
pub const PACK_FILE: &str = "frames.upk";

/// `<data_dir>/upscaled/frames.upk`.
pub fn pack_path(data_dir: &Path) -> PathBuf {
    data_dir.join("upscaled").join(PACK_FILE)
}

/// `<data_dir>/upscaled/cache`, the resume cache. The caller may delete it
/// after the pack has been promoted.
pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("upscaled").join("cache")
}

/// SHA-256 of `<data_dir>/game/import.json`, the source manifest hash that
/// ties a pack to one game import.
pub fn manifest_hash(data_dir: &Path) -> std::io::Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(data_dir.join("game").join("import.json"))?;
    Ok(Sha256::digest(&bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_hash_reads_import_json() {
        let d = tempfile::tempdir().unwrap();
        assert!(manifest_hash(d.path()).is_err());
        std::fs::create_dir(d.path().join("game")).unwrap();
        std::fs::write(d.path().join("game/import.json"), b"abc").unwrap();
        let h = manifest_hash(d.path()).unwrap();
        assert_eq!(h[..4], [0xba, 0x78, 0x16, 0xbf]);
        assert_eq!(pack_path(d.path()), d.path().join("upscaled/frames.upk"));
    }
}
