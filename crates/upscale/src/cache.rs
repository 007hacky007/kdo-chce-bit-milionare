//! Resume cache: one encoded upscaled frame per file, keyed by content hash.
//!
//! `<data_dir>/upscaled/cache/<32 hex digits>.bin`, record layout in
//! `docs/upscale-pack.md`. Every record is written to a temporary file in
//! the same directory and renamed into place, so a crash leaves either the
//! whole record or an orphan `.tmp` file that [`FrameCache::sweep_temp`]
//! removes. Invalid records are treated as absent and deleted.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::alpha::BlitMode;
use crate::key::ContentHash;
use crate::pack::{Codec, MAX_SIDE, PackError, Result, sweep_tmp, sync_dir};

const MAGIC: &[u8; 8] = b"MILUCACH";
const VERSION: u32 = 2;
const RECORD_HEADER_LEN: usize = 76;
/// Upper bound for a payload: raw RGBA at the largest frame size.
const MAX_PAYLOAD: u64 = MAX_SIDE as u64 * MAX_SIDE as u64 * 4;

/// An encoded upscaled frame as stored in the cache and the pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// The blit the alpha was made for.
    pub mode: BlitMode,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    pub payload: Vec<u8>,
}

/// The cache directory. Single writer; readers may run alongside.
#[derive(Debug)]
pub struct FrameCache {
    dir: PathBuf,
}

fn hex(hash: &ContentHash) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

impl FrameCache {
    /// Opens (creating if needed) the cache directory.
    pub fn open(dir: &Path) -> Result<FrameCache> {
        std::fs::create_dir_all(dir)?;
        Ok(FrameCache {
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, hash: &ContentHash) -> PathBuf {
        self.dir.join(format!("{}.bin", hex(hash)))
    }

    /// True when a record for `hash` made for blit `mode` exists and its
    /// header is valid and matches the file size. The payload checksum is
    /// verified by [`get`](Self::get).
    pub fn contains(&self, hash: &ContentHash, mode: BlitMode) -> bool {
        let Ok(mut f) = std::fs::File::open(self.path(hash)) else {
            return false;
        };
        let Ok(len) = f.metadata().map(|m| m.len()) else {
            return false;
        };
        let mut h = [0u8; RECORD_HEADER_LEN];
        if f.read_exact(&mut h).is_err() {
            return false;
        }
        parse_header(&h, hash)
            .is_ok_and(|r| r.0 == mode && RECORD_HEADER_LEN as u64 + u64::from(r.4) == len)
    }

    /// Stores `frame` for `hash` atomically, replacing any old record.
    pub fn put(&self, hash: &ContentHash, frame: &EncodedFrame) -> Result<()> {
        check_meta(frame.width, frame.height, frame.payload.len() as u64)?;
        let mut rec = Vec::with_capacity(RECORD_HEADER_LEN + frame.payload.len());
        rec.extend_from_slice(MAGIC);
        rec.extend_from_slice(&VERSION.to_le_bytes());
        rec.extend_from_slice(hash);
        rec.extend_from_slice(&frame.width.to_le_bytes());
        rec.extend_from_slice(&frame.height.to_le_bytes());
        rec.push(frame.codec.id());
        rec.push(frame.mode.id());
        rec.extend_from_slice(&[0; 2]);
        rec.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
        let sha = record_digest(&rec, &frame.payload);
        rec.extend_from_slice(&sha);
        rec.extend_from_slice(&frame.payload);
        let mut tmp = tempfile::Builder::new()
            .prefix(".")
            .suffix(".tmp")
            .tempfile_in(&self.dir)?;
        tmp.write_all(&rec)?;
        tmp.as_file().sync_all()?;
        let path = self.path(hash);
        tmp.persist(&path).map_err(|e| PackError::Io(e.error))?;
        sync_dir(&path);
        Ok(())
    }

    /// The record for `hash` made for blit `mode`, or `None` when absent,
    /// made for the other mode or invalid (such a record is deleted so the
    /// frame is redone).
    pub fn get(&self, hash: &ContentHash, mode: BlitMode) -> Option<EncodedFrame> {
        let path = self.path(hash);
        let bytes = std::fs::read(&path).ok()?;
        match parse_record(&bytes, hash).filter(|f| f.mode == mode) {
            Some(f) => Some(f),
            None => {
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Deletes orphan temporary files left by an interrupted [`put`](Self::put).
    pub fn sweep_temp(&self) -> Result<usize> {
        sweep_tmp(&self.dir, ".")
    }
}

fn check_meta(width: u32, height: u32, len: u64) -> Result<()> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || len > MAX_PAYLOAD {
        return Err(PackError::Invalid(format!(
            "cache record {width}x{height} with {len} bytes out of range"
        )));
    }
    Ok(())
}

/// (mode, width, height, codec, length, sha256) from a record header.
type RecordHeader = (BlitMode, u32, u32, Codec, u32, [u8; 32]);

fn parse_header(h: &[u8], hash: &ContentHash) -> Result<RecordHeader> {
    let bad = || PackError::Invalid("bad cache record".into());
    if h.len() < RECORD_HEADER_LEN || &h[0..8] != MAGIC {
        return Err(bad());
    }
    let u32_at = |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
    if u32_at(8) != VERSION || h[12..28] != hash[..] || h[38..40] != [0; 2] {
        return Err(bad());
    }
    let (width, height, len) = (u32_at(28), u32_at(32), u32_at(40));
    let codec = Codec::from_id(h[36]).ok_or_else(bad)?;
    let mode = BlitMode::from_id(h[37]).ok_or_else(bad)?;
    check_meta(width, height, u64::from(len))?;
    let mut sha = [0u8; 32];
    sha.copy_from_slice(&h[44..76]);
    Ok((mode, width, height, codec, len, sha))
}

/// SHA-256 over the first 44 header bytes and the payload, so metadata
/// corruption is caught as well.
fn record_digest(head: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(&head[..44]);
    h.update(payload);
    h.finalize().into()
}

fn parse_record(bytes: &[u8], hash: &ContentHash) -> Option<EncodedFrame> {
    let (mode, width, height, codec, len, sha) = parse_header(bytes, hash).ok()?;
    let payload = &bytes[RECORD_HEADER_LEN..];
    if payload.len() != len as usize || record_digest(&bytes[..44], payload) != sha {
        return None;
    }
    Some(EncodedFrame {
        mode,
        width,
        height,
        codec,
        payload: payload.to_vec(),
    })
}
