//! Frame identity: which Smacker frame an upscaled image belongs to.

use sha2::{Digest, Sha256};

/// Truncated SHA-256 of a source frame, see [`content_hash`].
pub type ContentHash = [u8; 16];

/// Domain prefix of [`content_hash`]; bump the last byte if the layout changes.
const HASH_PREFIX: &[u8; 16] = b"MILUPK-FRAME\0\0\0\x01";

/// A Smacker frame by file and position.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameKey {
    /// Lower-case path relative to `game/`, `/` separators.
    pub file: String,
    /// 1-based frame number.
    pub frame: u32,
}

impl FrameKey {
    /// Builds a key from a relative path in any case with `/` or `\`.
    pub fn new(file: &str, frame: u32) -> FrameKey {
        FrameKey {
            file: file.replace('\\', "/").to_ascii_lowercase(),
            frame,
        }
    }

    /// True when `file` is a normalised relative name the pack can store:
    /// 1..=255 bytes of printable ASCII without upper case, `\` or `:`,
    /// and every `/`-separated segment non-empty and not `.` or `..`.
    pub fn is_valid_name(file: &str) -> bool {
        !file.is_empty()
            && file.len() <= 255
            && file.bytes().all(|b| {
                (0x20..0x7f).contains(&b) && !b.is_ascii_uppercase() && b != b'\\' && b != b':'
            })
            && file
                .split('/')
                .all(|s| !s.is_empty() && s != "." && s != "..")
    }
}

/// Identity of a decoded source frame: the first 16 bytes of SHA-256 over
/// `HASH_PREFIX`, `width` (u32 LE), `height` (u32 LE), the 256 palette
/// entries as R, G, B bytes, then the `width * height` palette indices.
pub fn content_hash(
    pixels: &[u8],
    palette: &[[u8; 3]; 256],
    width: u32,
    height: u32,
) -> ContentHash {
    let mut h = Sha256::new();
    h.update(HASH_PREFIX);
    h.update(width.to_le_bytes());
    h.update(height.to_le_bytes());
    for c in palette {
        h.update(c);
    }
    h.update(pixels);
    let full: [u8; 32] = h.finalize().into();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}
