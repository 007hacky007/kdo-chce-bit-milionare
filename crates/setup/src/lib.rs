//! Turns a user's ISO image, `Setup.exe` or installed game folder into a
//! verified data directory (`<data_dir>/game/...`).

mod import;
mod manifest;
#[rustfmt::skip]
mod manifest_data;
mod source;
mod verify;

use std::io;
use std::path::PathBuf;

pub use import::{ImportReport, PlayingLock, Progress, import, recover};
pub use manifest::{Manifest, ManifestEntry};
pub use manifest_data::TOPCD;
pub use source::{Source, detect_source};
pub use verify::verify;

/// Everything that can go wrong while detecting or importing a source.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("not a recognized game source: {}", .0.display())]
    NotRecognized(PathBuf),
    #[error("{file} does not match the expected release (expected {expected}, got {actual})")]
    Mismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("missing file in source: {0}")]
    Missing(String),
    #[error("not enough free space: need {needed} bytes, {available} available")]
    NoSpace { needed: u64, available: u64 },
    #[error("another import is already running for {}", .0.display())]
    Busy(PathBuf),
    #[error("the game is running from {}; quit the game first", .0.display())]
    GameRunning(PathBuf),
    #[error("import cancelled")]
    Cancelled,
    #[error("I/O error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid disc image {}: {source}", path.display())]
    Format {
        path: PathBuf,
        #[source]
        source: formats::FormatError,
    },
    #[error(
        "incomplete disc image {}: {actual} of {expected} bytes (truncated copy or download?)",
        path.display()
    )]
    Incomplete {
        path: PathBuf,
        actual: u64,
        expected: u64,
    },
    #[error("refusing to replace {}: it was not created by an import", .0.display())]
    ForeignGameDir(PathBuf),
}

impl ImportError {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> ImportError {
        let path = path.into();
        move |source| ImportError::Io { path, source }
    }

    /// Attributes a disc image decoding error to the image file. Read
    /// failures of the underlying file stay I/O errors.
    pub(crate) fn image(
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(formats::FormatError) -> ImportError {
        let path = path.into();
        move |e| match e {
            formats::FormatError::Io { source, .. } => ImportError::Io { path, source },
            source => ImportError::Format { path, source },
        }
    }
}
