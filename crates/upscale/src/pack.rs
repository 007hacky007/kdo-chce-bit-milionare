//! The upscale pack: one indexed file of upscaled RGBA frames.
//!
//! Byte layout: `docs/upscale-pack.md`. All integers are little-endian.
//! Readers validate every field and never panic on malformed input; writers
//! stage into a temporary file next to the target and rename it into place.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::alpha::BlitMode;
use crate::key::{ContentHash, FrameKey};

/// Pack format version written and accepted by this code.
pub const FORMAT_VERSION: u32 = 2;

const MAGIC: &[u8; 8] = b"MILUPACK";
const HEADER_LEN: usize = 256;
const ENTRY_LEN: usize = 52;
const MODEL_NAME_LEN: usize = 32;
/// Largest accepted frame side (the game is 640x480, so 3x is 1920x1440).
pub const MAX_SIDE: u32 = 2048;
const _: () = assert!(MAX_SIDE <= u16::MAX as u32);
/// Largest accepted index (the full game needs well under 1 MB).
const MAX_INDEX_LEN: u64 = 64 << 20;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("pack i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not an upscale pack (bad magic)")]
    BadMagic,
    #[error("unsupported pack format version {found} (expected {FORMAT_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("pack was made from a different game import (source manifest hash mismatch)")]
    ManifestMismatch,
    #[error("pack was made by upscale pipeline version {found} (expected {expected}); rebuild it")]
    PipelineMismatch { found: u32, expected: u32 },
    #[error("truncated pack: {0}")]
    Truncated(&'static str),
    #[error("invalid pack: {0}")]
    Invalid(String),
    /// Well formed, but made in a way this build does not use (scale, runner, model).
    #[error("incompatible pack: {0}")]
    Incompatible(String),
}

pub type Result<T> = std::result::Result<T, PackError>;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(PackError::Invalid(msg.into()))
}

/// Payload codec of one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Uncompressed RGBA, 4 bytes per pixel, rows top to bottom.
    Raw,
    /// Baseline JPEG (4:4:4, quality [`JPEG_QUALITY`]) of the RGB channels
    /// followed by a run-length coded binary alpha mask.
    JpegRle,
}

/// JPEG quality used by [`Codec::JpegRle`] (chosen by the codec pilot, see docs/upscale-pack.md).
pub const JPEG_QUALITY: u8 = 95;

impl Codec {
    /// Codec used for new packs.
    pub const DEFAULT: Codec = Codec::JpegRle;

    pub fn id(self) -> u8 {
        match self {
            Codec::Raw => 0,
            Codec::JpegRle => 1,
        }
    }

    pub fn from_id(id: u8) -> Option<Codec> {
        match id {
            0 => Some(Codec::Raw),
            1 => Some(Codec::JpegRle),
            _ => None,
        }
    }
}

/// Encodes an RGBA frame with `codec`. Alpha must be 0 or 255. For
/// [`Codec::JpegRle`] the RGB under transparent pixels is encoded as given,
/// so callers should leave the upscaler's smooth fill there rather than
/// black, which would ring into visible edge pixels.
pub fn encode(codec: Codec, frame: &RgbaFrame) -> Result<Vec<u8>> {
    check_frame(frame)?;
    if frame
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[3] != 0 && p[3] != 255)
    {
        return invalid("alpha must be 0 or 255");
    }
    match codec {
        Codec::Raw => {
            let mut out = frame.rgba.clone();
            for p in out.as_chunks_mut::<4>().0 {
                if p[3] == 0 {
                    *p = [0; 4];
                }
            }
            Ok(out)
        }
        Codec::JpegRle => encode_jpeg_rle(frame),
    }
}

/// Decodes a payload that must hold a `width` x `height` RGBA frame.
/// Transparent pixels come back as `[0, 0, 0, 0]`, visible ones with
/// alpha 255.
pub fn decode(codec: Codec, width: u32, height: u32, payload: &[u8]) -> Result<RgbaFrame> {
    check_dims(width, height)?;
    let n = rgba_len(width, height);
    let rgba = match codec {
        Codec::Raw => {
            if payload.len() != n {
                return invalid(format!(
                    "raw payload has {} bytes, expected {n}",
                    payload.len()
                ));
            }
            let mut v = payload.to_vec();
            for p in v.as_chunks_mut::<4>().0 {
                match p[3] {
                    0 => *p = [0; 4],
                    255 => {}
                    _ => return invalid("raw payload alpha must be 0 or 255"),
                }
            }
            v
        }
        Codec::JpegRle => decode_jpeg_rle(width, height, payload)?,
    };
    Ok(RgbaFrame {
        width,
        height,
        rgba,
    })
}

fn encode_jpeg_rle(frame: &RgbaFrame) -> Result<Vec<u8>> {
    let rgb: Vec<u8> = frame
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let mut jpeg = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut jpeg, JPEG_QUALITY);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
    // Standard Huffman tables: zune-jpeg 0.5.15 mis-decodes some files
    // written by jpeg-encoder 0.7.1 with optimized tables (libjpeg reads
    // them fine), see `jpeg_rle_small_and_flat_frames`.
    enc.set_optimized_huffman_tables(false);
    // MAX_SIDE keeps both sides within u16.
    enc.encode(
        &rgb,
        frame.width as u16,
        frame.height as u16,
        jpeg_encoder::ColorType::Rgb,
    )
    .map_err(|e| PackError::Invalid(format!("jpeg encode: {e}")))?;
    let jpeg_len = u32::try_from(jpeg.len())
        .map_err(|_| PackError::Invalid("jpeg larger than 4 GiB".into()))?;
    let mut out = Vec::with_capacity(jpeg.len() + 4 + 1024);
    out.extend_from_slice(&jpeg_len.to_le_bytes());
    out.extend_from_slice(&jpeg);
    // Alternating runs, transparent first (possibly 0).
    let mut visible = false;
    let mut run = 0u32;
    for p in frame.rgba.as_chunks::<4>().0 {
        if (p[3] != 0) == visible {
            run += 1;
        } else {
            put_varint(&mut out, run);
            visible = !visible;
            run = 1;
        }
    }
    put_varint(&mut out, run);
    Ok(out)
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(b: &[u8], at: &mut usize) -> Result<u32> {
    let mut v = 0u64;
    for shift in (0..35).step_by(7) {
        let Some(&byte) = b.get(*at) else {
            return invalid("alpha runs truncated");
        };
        *at += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return u32::try_from(v).map_err(|_| PackError::Invalid("alpha run too long".into()));
        }
    }
    invalid("alpha run varint too long")
}

fn decode_jpeg_rle(width: u32, height: u32, payload: &[u8]) -> Result<Vec<u8>> {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let Some(len_bytes) = payload.get(..4) else {
        return invalid("jpeg payload truncated");
    };
    let jpeg_len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let Some(jpeg_end) = (jpeg_len as usize)
        .checked_add(4)
        .filter(|&e| e <= payload.len())
    else {
        return invalid("jpeg payload truncated");
    };
    let (jpeg, runs) = (&payload[4..jpeg_end], &payload[jpeg_end..]);
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(width as usize)
        .set_max_height(height as usize)
        .set_strict_mode(true);
    // zune-jpeg is fuzzed, but a decoder panic must still not take the game
    // down: it becomes an error (and the frame falls back). This only helps
    // with panic = "unwind", the default profile setting.
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<u8>> {
        let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
        dec.decode_headers()
            .map_err(|e| PackError::Invalid(format!("jpeg header: {e:?}")))?;
        match dec.info() {
            Some(i) if (u32::from(i.width), u32::from(i.height)) == (width, height) => {}
            _ => return invalid("jpeg dimensions do not match the entry"),
        }
        dec.decode()
            .map_err(|e| PackError::Invalid(format!("jpeg decode: {e:?}")))
    }));
    let mut rgba = match decoded {
        Ok(r) => r?,
        Err(_) => return invalid("jpeg decoder panicked"),
    };
    let n = width as usize * height as usize;
    if rgba.len() != n * 4 {
        return invalid("jpeg decoded to an unexpected size");
    }
    let mut at = 0usize;
    let mut px = 0usize;
    let mut visible = false;
    while at < runs.len() {
        let run = get_varint(runs, &mut at)? as usize;
        // Only the first (transparent) run may be empty.
        if run == 0 && (visible || px != 0) {
            return invalid("empty interior alpha run");
        }
        let end = px
            .checked_add(run)
            .filter(|&e| e <= n)
            .ok_or(PackError::Invalid("alpha runs exceed the frame".into()))?;
        let span = &mut rgba[px * 4..end * 4];
        if visible {
            span.as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|p| p[3] = 255);
        } else {
            span.fill(0);
        }
        px = end;
        visible = !visible;
    }
    if px != n {
        return invalid("alpha runs do not cover the frame");
    }
    Ok(rgba)
}

fn rgba_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

fn check_dims(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return invalid(format!("frame size {width}x{height} out of range"));
    }
    Ok(())
}

fn check_frame(frame: &RgbaFrame) -> Result<()> {
    check_dims(frame.width, frame.height)?;
    if frame.rgba.len() != rgba_len(frame.width, frame.height) {
        return invalid(format!(
            "{}x{} frame has {} bytes of RGBA",
            frame.width,
            frame.height,
            frame.rgba.len()
        ));
    }
    Ok(())
}

fn payload_digest(payload: &[u8]) -> [u8; 8] {
    let d = Sha256::digest(payload);
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[..8]);
    out
}

/// Fixed upscaler facts recorded in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerParams {
    pub runner_sha256: [u8; 32],
    pub model_param_sha256: [u8; 32],
    pub model_bin_sha256: [u8; 32],
    /// Printable ASCII, 1..=31 bytes.
    pub model_name: String,
    pub scale: u32,
    pub tile: u32,
    pub tta: bool,
    pub gpu: i32,
}

/// Header fields chosen by the writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackHeader {
    pub params: RunnerParams,
    /// SHA-256 of `<data_dir>/game/import.json`.
    pub source_manifest_sha256: [u8; 32],
    /// [`crate::pipeline::PIPELINE_VERSION`] of the pipeline that made the
    /// frames (fill, mask and blit rule).
    pub pipeline_version: u32,
}

/// One frame entry of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 1-based frame number.
    pub frame: u32,
    /// [`crate::content_hash`] of the source frame.
    pub content_hash: ContentHash,
    /// Upscaled width and height in pixels.
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    /// Absolute file offset and length of the encoded payload.
    pub offset: u64,
    pub length: u32,
    /// First 8 bytes of SHA-256 of the payload.
    pub payload_sha256: [u8; 8],
}

/// A decoded upscaled frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, alpha 0 or 255.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
struct Blob {
    mode: BlitMode,
    width: u32,
    height: u32,
    codec: Codec,
    offset: u64,
    length: u32,
    digest: [u8; 8],
}

/// Writes a pack into a temporary file and renames it over `path` on
/// [`finish`](Self::finish). Dropping an unfinished writer deletes the
/// temporary file and leaves any existing pack untouched.
pub struct PackWriter {
    out: BufWriter<tempfile::NamedTempFile>,
    path: std::path::PathBuf,
    header: PackHeader,
    codec: Codec,
    pos: u64,
    /// Payloads by content hash and the blit their alpha was made for.
    blobs: HashMap<(ContentHash, BlitMode), Blob>,
    entries: BTreeMap<FrameKey, Entry>,
    modes: BTreeMap<String, BlitMode>,
}

impl PackWriter {
    /// Starts a pack that will replace `path`; new frames use `codec`.
    ///
    /// Stale staging files of earlier crashed writers (`.<name>.*.tmp` next
    /// to `path`) are deleted first, so only one writer per pack may run at
    /// a time (the pipeline holds `upscaled/.upscale.lock` for that). On Windows the final rename fails while another process (the
    /// game) holds the old pack open; the caller must make sure it does not.
    pub fn create(path: &Path, header: &PackHeader, codec: Codec) -> Result<PackWriter> {
        check_params(&header.params)?;
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        sweep_tmp(dir, &format!(".{name}."))?;
        let tmp = tempfile::Builder::new()
            .prefix(&format!(".{name}."))
            .suffix(".tmp")
            .tempfile_in(dir)?;
        let mut out = BufWriter::with_capacity(1 << 20, tmp);
        out.write_all(&[0u8; HEADER_LEN])?;
        Ok(PackWriter {
            out,
            path: path.to_path_buf(),
            header: header.clone(),
            codec,
            pos: HEADER_LEN as u64,
            blobs: HashMap::new(),
            entries: BTreeMap::new(),
            modes: BTreeMap::new(),
        })
    }

    /// Encodes and adds one frame made for blit `mode` (every frame of a
    /// file must have the same mode). A content hash already in the pack
    /// with the same mode reuses the stored payload.
    pub fn add_frame(
        &mut self,
        key: &FrameKey,
        content_hash: ContentHash,
        mode: BlitMode,
        frame: &RgbaFrame,
    ) -> Result<()> {
        check_frame(frame)?;
        self.check_key(key, mode)?;
        if let Some(b) = self.blobs.get(&(content_hash, mode)).copied() {
            return self.insert(key, content_hash, b, frame.width, frame.height);
        }
        let payload = encode(self.codec, frame)?;
        self.add_encoded(
            key,
            content_hash,
            mode,
            frame.width,
            frame.height,
            self.codec,
            &payload,
        )
    }

    /// Adds a frame from the resume cache.
    pub fn add_cached(
        &mut self,
        key: &FrameKey,
        content_hash: ContentHash,
        frame: &crate::cache::EncodedFrame,
    ) -> Result<()> {
        self.add_encoded(
            key,
            content_hash,
            frame.mode,
            frame.width,
            frame.height,
            frame.codec,
            &frame.payload,
        )
    }

    /// Adds an already encoded payload (e.g. from a resume cache) made for
    /// blit `mode`. The payload is not decoded here; readers reject it if it
    /// is bad.
    #[allow(clippy::too_many_arguments)]
    pub fn add_encoded(
        &mut self,
        key: &FrameKey,
        content_hash: ContentHash,
        mode: BlitMode,
        width: u32,
        height: u32,
        codec: Codec,
        payload: &[u8],
    ) -> Result<()> {
        check_dims(width, height)?;
        self.check_key(key, mode)?;
        // A wrong raw size would make the whole index invalid, not one frame.
        if codec == Codec::Raw && payload.len() != rgba_len(width, height) {
            return invalid("raw payload size does not match the dimensions");
        }
        if let Some(b) = self.blobs.get(&(content_hash, mode)).copied() {
            return self.insert(key, content_hash, b, width, height);
        }
        let length = u32::try_from(payload.len())
            .map_err(|_| PackError::Invalid("payload larger than 4 GiB".into()))?;
        let blob = Blob {
            mode,
            width,
            height,
            codec,
            offset: self.pos,
            length,
            digest: payload_digest(payload),
        };
        self.out.write_all(payload)?;
        self.pos += u64::from(length);
        self.blobs.insert((content_hash, mode), blob);
        self.insert(key, content_hash, blob, width, height)
    }

    fn check_key(&self, key: &FrameKey, mode: BlitMode) -> Result<()> {
        if !FrameKey::is_valid_name(&key.file) {
            return invalid(format!("bad file name {:?}", key.file));
        }
        if key.frame == 0 {
            return invalid("frame numbers are 1-based");
        }
        if self.entries.contains_key(key) {
            return invalid(format!("duplicate entry {} frame {}", key.file, key.frame));
        }
        if self.modes.get(&key.file).is_some_and(|m| *m != mode) {
            return invalid(format!("{}: frames for both blit modes", key.file));
        }
        Ok(())
    }

    fn insert(
        &mut self,
        key: &FrameKey,
        content_hash: ContentHash,
        b: Blob,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if (b.width, b.height) != (width, height) {
            return invalid("same content hash with different dimensions");
        }
        if self.entries.len() >= u32::MAX as usize {
            return invalid("too many entries");
        }
        self.modes.insert(key.file.clone(), b.mode);
        self.entries.insert(
            key.clone(),
            Entry {
                frame: key.frame,
                content_hash,
                width,
                height,
                codec: b.codec,
                offset: b.offset,
                length: b.length,
                payload_sha256: b.digest,
            },
        );
        Ok(())
    }

    /// Writes the index and header, syncs, and renames into place.
    pub fn finish(self) -> Result<()> {
        let mut index = Vec::new();
        let mut files: Vec<(&str, Vec<&Entry>)> = Vec::new();
        for (k, e) in &self.entries {
            match files.last_mut() {
                Some((f, v)) if *f == k.file => v.push(e),
                _ => files.push((&k.file, vec![e])),
            }
        }
        for (name, entries) in &files {
            index.extend_from_slice(&(name.len() as u16).to_le_bytes());
            index.extend_from_slice(name.as_bytes());
            index.push(self.modes[*name].id());
            index.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            for e in entries {
                index.extend_from_slice(&e.frame.to_le_bytes());
                index.extend_from_slice(&e.content_hash);
                index.extend_from_slice(&e.width.to_le_bytes());
                index.extend_from_slice(&e.height.to_le_bytes());
                index.push(e.codec.id());
                index.extend_from_slice(&[0; 3]);
                index.extend_from_slice(&e.offset.to_le_bytes());
                index.extend_from_slice(&e.length.to_le_bytes());
                index.extend_from_slice(&e.payload_sha256);
            }
        }
        let file_count =
            u32::try_from(files.len()).map_err(|_| PackError::Invalid("too many files".into()))?;
        let header = encode_header(
            &self.header,
            self.pos,
            index.len() as u64,
            Sha256::digest(&index).into(),
            file_count,
            self.entries.len() as u32,
        );
        let mut out = self.out;
        out.write_all(&index)?;
        let mut tmp = out
            .into_inner()
            .map_err(|e| PackError::Io(e.into_error()))?;
        tmp.seek(SeekFrom::Start(0))?;
        tmp.write_all(&header)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path)
            .map_err(|e| PackError::Io(e.error))?;
        sync_dir(&self.path);
        Ok(())
    }
}

/// Deletes files in `dir` named `<prefix>*.tmp`; returns how many. A
/// missing directory counts as empty; files that cannot be removed (e.g.
/// still open on Windows) are skipped.
pub(crate) fn sweep_tmp(dir: &Path, prefix: &str) -> Result<usize> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut n = 0;
    for e in rd {
        let e = e?;
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(prefix)
            && name.ends_with(".tmp")
            && e.file_type().is_ok_and(|t| t.is_file())
            && std::fs::remove_file(e.path()).is_ok()
        {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(unix)]
pub(crate) fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent()
        && let Ok(d) = File::open(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        })
    {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
pub(crate) fn sync_dir(_path: &Path) {}

fn check_params(p: &RunnerParams) -> Result<()> {
    let n = p.model_name.as_bytes();
    if n.is_empty() || n.len() >= MODEL_NAME_LEN || !n.iter().all(|b| (0x20..0x7f).contains(b)) {
        return invalid(format!("bad model name {:?}", p.model_name));
    }
    Ok(())
}

fn encode_header(
    h: &PackHeader,
    index_offset: u64,
    index_len: u64,
    index_sha256: [u8; 32],
    file_count: u32,
    entry_count: u32,
) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[0..8].copy_from_slice(MAGIC);
    b[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    b[12..16].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    b[16..48].copy_from_slice(&h.params.runner_sha256);
    b[48..80].copy_from_slice(&h.params.model_param_sha256);
    b[80..112].copy_from_slice(&h.params.model_bin_sha256);
    b[112..144].copy_from_slice(&h.source_manifest_sha256);
    let name = h.params.model_name.as_bytes();
    b[144..144 + name.len()].copy_from_slice(name);
    b[176..180].copy_from_slice(&h.params.scale.to_le_bytes());
    b[180..184].copy_from_slice(&h.params.tile.to_le_bytes());
    b[184..188].copy_from_slice(&u32::from(h.params.tta).to_le_bytes());
    b[188..192].copy_from_slice(&h.params.gpu.to_le_bytes());
    b[192..200].copy_from_slice(&index_offset.to_le_bytes());
    b[200..208].copy_from_slice(&index_len.to_le_bytes());
    b[208..240].copy_from_slice(&index_sha256);
    b[240..244].copy_from_slice(&file_count.to_le_bytes());
    b[244..248].copy_from_slice(&entry_count.to_le_bytes());
    b[248..252].copy_from_slice(&h.pipeline_version.to_le_bytes());
    b
}

/// Bounds-checked little-endian cursor; every read fails instead of panicking.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or(PackError::Invalid("index ends inside a record".into()))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
}

/// A validated, open pack. Payloads are read lazily.
pub struct PackReader {
    file: Mutex<File>,
    header: PackHeader,
    entries: BTreeMap<FrameKey, Entry>,
    modes: HashMap<String, BlitMode>,
}

impl std::fmt::Debug for PackReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackReader")
            .field("header", &self.header)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl PackReader {
    /// Opens and validates a pack made from the import whose
    /// `game/import.json` hashes to `source_manifest_sha256`.
    ///
    /// The pipeline version, runner and model hashes, scale, tile, TTA and
    /// GPU are not compared here: the game checks pipeline version and scale
    /// (`render::UpscaledPack::open`), the pipeline deciding whether to
    /// rebuild compares the whole header.
    pub fn open(path: &Path, source_manifest_sha256: &[u8; 32]) -> Result<PackReader> {
        Self::open_inner(path, Some(source_manifest_sha256))
    }

    /// Opens and validates a pack made from any import (the pipeline seeds
    /// its cache from an older pack; frames are matched by content hash).
    pub fn open_any_import(path: &Path) -> Result<PackReader> {
        Self::open_inner(path, None)
    }

    fn open_inner(path: &Path, source_manifest_sha256: Option<&[u8; 32]>) -> Result<PackReader> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut hb = [0u8; HEADER_LEN];
        if file_len < HEADER_LEN as u64 {
            return Err(PackError::Truncated("header"));
        }
        file.read_exact(&mut hb)?;
        let mut c = Cur { b: &hb, at: 0 };
        if c.arr::<8>()? != *MAGIC {
            return Err(PackError::BadMagic);
        }
        let version = c.u32()?;
        if version != FORMAT_VERSION {
            return Err(PackError::UnsupportedVersion { found: version });
        }
        if c.u32()? != HEADER_LEN as u32 {
            return invalid("header length");
        }
        let runner_sha256 = c.arr()?;
        let model_param_sha256 = c.arr()?;
        let model_bin_sha256 = c.arr()?;
        let manifest: [u8; 32] = c.arr()?;
        let name_raw: [u8; MODEL_NAME_LEN] = c.arr()?;
        let name_len = name_raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(MODEL_NAME_LEN);
        if name_raw[name_len..].iter().any(|&b| b != 0) {
            return invalid("model name padding");
        }
        let model_name = String::from_utf8_lossy(&name_raw[..name_len]).into_owned();
        let scale = c.u32()?;
        let tile = c.u32()?;
        let tta = match c.u32()? {
            0 => false,
            1 => true,
            _ => return invalid("tta flag"),
        };
        let gpu = c.u32()? as i32;
        let index_offset = c.u64()?;
        let index_len = c.u64()?;
        let index_sha256: [u8; 32] = c.arr()?;
        let file_count = c.u32()?;
        let entry_count = c.u32()?;
        let pipeline_version = c.u32()?;
        if c.take(4)?.iter().any(|&b| b != 0) {
            return invalid("reserved header bytes");
        }
        let params = RunnerParams {
            runner_sha256,
            model_param_sha256,
            model_bin_sha256,
            model_name,
            scale,
            tile,
            tta,
            gpu,
        };
        check_params(&params)?;
        if source_manifest_sha256.is_some_and(|m| manifest != *m) {
            return Err(PackError::ManifestMismatch);
        }
        if index_offset < HEADER_LEN as u64 || index_len > MAX_INDEX_LEN {
            return invalid("index location");
        }
        match index_offset.checked_add(index_len) {
            Some(end) if end == file_len => {}
            Some(end) if end > file_len => return Err(PackError::Truncated("index")),
            _ => return invalid("trailing bytes after index"),
        }
        file.seek(SeekFrom::Start(index_offset))?;
        let mut index = vec![0u8; index_len as usize];
        file.read_exact(&mut index)?;
        if <[u8; 32]>::from(Sha256::digest(&index)) != index_sha256 {
            return invalid("index checksum mismatch");
        }
        let (entries, modes) = parse_index(&index, index_offset, file_count, entry_count)?;
        Ok(PackReader {
            file: Mutex::new(file),
            header: PackHeader {
                params,
                source_manifest_sha256: manifest,
                pipeline_version,
            },
            entries,
            modes,
        })
    }

    pub fn header(&self) -> &PackHeader {
        &self.header
    }

    /// Number of frame entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All keys, sorted by file then frame.
    pub fn keys(&self) -> impl Iterator<Item = &FrameKey> {
        self.entries.keys()
    }

    pub fn entry(&self, key: &FrameKey) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// The blit the frames of `file` (a normalised key name) were made for.
    pub fn blit_mode(&self, file: &str) -> Option<BlitMode> {
        self.modes.get(file).copied()
    }

    /// Reads and checks the encoded payload of `entry`.
    pub fn read_payload(&self, entry: &Entry) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; entry.length as usize];
        {
            let mut f = self
                .file
                .lock()
                .map_err(|_| PackError::Invalid("pack file lock poisoned".into()))?;
            f.seek(SeekFrom::Start(entry.offset))?;
            f.read_exact(&mut buf)?;
        }
        if payload_digest(&buf) != entry.payload_sha256 {
            return invalid("payload checksum mismatch");
        }
        Ok(buf)
    }

    /// The upscaled frame for `key` when the pack holds one made from a source
    /// frame with `content_hash`; `Ok(None)` when absent or stale.
    pub fn read_frame(
        &self,
        key: &FrameKey,
        content_hash: &ContentHash,
    ) -> Result<Option<RgbaFrame>> {
        let Some(e) = self.entries.get(key) else {
            return Ok(None);
        };
        if e.content_hash != *content_hash {
            return Ok(None);
        }
        let payload = self.read_payload(e)?;
        decode(e.codec, e.width, e.height, &payload).map(Some)
    }
}

fn parse_index(
    index: &[u8],
    payload_end: u64,
    file_count: u32,
    entry_count: u32,
) -> Result<(BTreeMap<FrameKey, Entry>, HashMap<String, BlitMode>)> {
    let mut c = Cur { b: index, at: 0 };
    let mut entries = BTreeMap::new();
    let mut modes = HashMap::new();
    let mut prev_file: Option<String> = None;
    for _ in 0..file_count {
        let n = c.u16()? as usize;
        let name = std::str::from_utf8(c.take(n)?)
            .ok()
            .filter(|s| FrameKey::is_valid_name(s))
            .ok_or(PackError::Invalid("bad file name in index".into()))?
            .to_string();
        if prev_file.as_ref().is_some_and(|p| *p >= name) {
            return invalid("index files not strictly sorted");
        }
        let mode = BlitMode::from_id(c.arr::<1>()?[0])
            .ok_or(PackError::Invalid("unknown blit mode".into()))?;
        modes.insert(name.clone(), mode);
        let count = c.u32()?;
        if count == 0 || u64::from(count) * ENTRY_LEN as u64 > (index.len() - c.at) as u64 {
            return invalid("file entry count");
        }
        let mut prev_frame = 0u32;
        for _ in 0..count {
            let frame = c.u32()?;
            let content_hash = c.arr()?;
            let width = c.u32()?;
            let height = c.u32()?;
            let codec = Codec::from_id(c.arr::<1>()?[0])
                .ok_or(PackError::Invalid("unknown codec".into()))?;
            if c.take(3)?.iter().any(|&b| b != 0) {
                return invalid("reserved entry bytes");
            }
            let offset = c.u64()?;
            let length = c.u32()?;
            let payload_sha256 = c.arr()?;
            if frame <= prev_frame {
                return invalid("index frames not strictly increasing from 1");
            }
            prev_frame = frame;
            check_dims(width, height)?;
            let in_bounds = offset >= HEADER_LEN as u64
                && offset
                    .checked_add(u64::from(length))
                    .is_some_and(|end| end <= payload_end);
            if !in_bounds {
                return invalid(format!("{name} frame {frame} payload out of bounds"));
            }
            if codec == Codec::Raw && length as usize != rgba_len(width, height) {
                return invalid(format!("{name} frame {frame} raw payload size"));
            }
            entries.insert(
                FrameKey {
                    file: name.clone(),
                    frame,
                },
                Entry {
                    frame,
                    content_hash,
                    width,
                    height,
                    codec,
                    offset,
                    length,
                    payload_sha256,
                },
            );
        }
        prev_file = Some(name);
    }
    if c.at != index.len() {
        return invalid("trailing bytes in index");
    }
    if entries.len() != entry_count as usize {
        return invalid("entry count mismatch");
    }
    Ok((entries, modes))
}
