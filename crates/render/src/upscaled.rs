//! Runtime source of AI-upscaled frames: the pack written by `crates/upscale`, decoded
//! lazily and kept in a byte-bounded LRU.
//!
//! A frame is used only when everything about it matches what the game is about to draw
//! (key, content hash, blit mode, 3x size); anything else is a [`Miss`] and the
//! compositor draws the original pixels instead (Review Focus 1).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use upscale::{Entry, PackError, PackReader};

pub use upscale::{BlitMode, ContentHash, FrameKey, PACK_FILE};

/// Default budget of decoded RGBA kept in memory (Review Focus 3).
pub const DEFAULT_BUDGET: usize = 256 << 20;

/// Scale the upscaled art is made and drawn at.
pub const UPSCALE: u32 = 3;

/// A decoded upscaled frame: `width * height` RGBA pixels, alpha 0 or 255.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame3x {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// No pixel has alpha 0 (rows can be copied whole).
    pub opaque: bool,
}

impl RgbaFrame3x {
    fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

/// What a pack must have been made with to be used by this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackExpectation {
    /// `upscale::pipeline::PIPELINE_VERSION` of the pipeline that made the frames.
    pub pipeline_version: u32,
    pub scale: u32,
    /// Require the pinned runner, model and runner parameters
    /// ([`upscale::runner::is_pinned`]), rejecting packs made by the test stand-in.
    pub pinned_runner: bool,
}

impl PackExpectation {
    /// The pipeline version and scale of this build, any runner.
    pub fn current() -> PackExpectation {
        PackExpectation {
            pipeline_version: upscale::pipeline::PIPELINE_VERSION,
            scale: UPSCALE,
            pinned_runner: false,
        }
    }

    /// [`current`](Self::current) plus the pinned runner and model (what the game uses).
    pub fn pinned() -> PackExpectation {
        PackExpectation {
            pinned_runner: true,
            ..PackExpectation::current()
        }
    }
}

/// Why a draw falls back to the original pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Miss {
    /// The frame source gave no file name or content hash.
    NoIdentity,
    /// The pack has no entry for this file and frame.
    NoEntry,
    /// The entry was made from a different source frame.
    StaleHash,
    /// The entry was made for the other blit (opaque or transparent).
    BlitMode,
    /// The entry is not 3x the source frame.
    Size,
    /// The payload failed its checksum or did not decode.
    Decode,
}

impl Miss {
    pub fn describe(self) -> &'static str {
        match self {
            Miss::NoIdentity => "no frame identity",
            Miss::NoEntry => "not in the pack",
            Miss::StaleHash => "pack entry made from a different frame",
            Miss::BlitMode => "pack entry made for the other blit mode",
            Miss::Size => "pack entry has the wrong size",
            Miss::Decode => "pack entry does not decode",
        }
    }
}

/// One draw to resolve: the source frame's identity and shape and the draw's blit.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Normalised file name (lower case, relative to `game/`, `/` separators).
    pub file: &'a str,
    /// 1-based.
    pub frame: u32,
    pub hash: ContentHash,
    pub src_width: u32,
    pub src_height: u32,
    pub transparent: bool,
    /// The frame comes from a cutscene stream slot (shown once): it is kept only for the
    /// compose that draws it, never in the LRU, so cutscenes do not evict backgrounds.
    pub stream: bool,
}

struct Slot {
    frame: Arc<RgbaFrame3x>,
    last_use: u64,
}

#[derive(Default)]
struct Lru {
    budget: usize,
    used: usize,
    clock: u64,
    /// Decoded frames by payload offset (identical frames share one payload).
    slots: HashMap<u64, Slot>,
    /// Payload offsets that failed to decode; never retried.
    failed: HashSet<u64>,
    decodes: u64,
    /// Stream-slot frames of the current compose (outside the budget; replaced at the
    /// next [`UpscaledPack::prefetch`]).
    stream: HashMap<u64, Arc<RgbaFrame3x>>,
}

/// Stream frames held at most when no prefetch marks the frame boundary.
const MAX_STREAM_HELD: usize = 4;

/// An open, validated pack plus the decoded-frame LRU. `Send + Sync`; decoding happens
/// on the calling thread (the compositor's).
pub struct UpscaledPack {
    reader: PackReader,
    lru: Mutex<Lru>,
}

impl std::fmt::Debug for UpscaledPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpscaledPack")
            .field("entries", &self.reader.len())
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl UpscaledPack {
    /// Opens `<dir>/frames.upk` (normally `<data_dir>/upscaled`). Fails when the pack is
    /// missing or malformed, was made from another import (`manifest_hash` is the SHA-256
    /// of `game/import.json`), by another pipeline version, at another scale or (when
    /// `expect.pinned_runner`) by another runner or model.
    pub fn open(
        dir: &Path,
        manifest_hash: &[u8; 32],
        expect: &PackExpectation,
    ) -> Result<UpscaledPack, PackError> {
        let reader = PackReader::open(&dir.join(PACK_FILE), manifest_hash)?;
        let h = reader.header();
        if h.pipeline_version != expect.pipeline_version {
            return Err(PackError::PipelineMismatch {
                found: h.pipeline_version,
                expected: expect.pipeline_version,
            });
        }
        if h.params.scale != expect.scale {
            return Err(PackError::Incompatible(format!(
                "pack is made at scale {}, expected {}",
                h.params.scale, expect.scale
            )));
        }
        if expect.pinned_runner && !upscale::runner::is_pinned(&h.params) {
            return Err(PackError::Incompatible(
                "pack was not made with the pinned Real-ESRGAN runner and model".into(),
            ));
        }
        Ok(UpscaledPack {
            reader,
            lru: Mutex::new(Lru {
                budget: DEFAULT_BUDGET,
                ..Lru::default()
            }),
        })
    }

    /// Changes the LRU budget (bytes of decoded RGBA), evicting as needed. A budget of
    /// 0 frees every decoded frame, the held stream frames too (the app does that while
    /// the original art is shown).
    pub fn set_budget(&self, bytes: usize) {
        let mut lru = lock(&self.lru);
        lru.budget = bytes;
        lru.make_room(0);
        if bytes == 0 {
            lru.stream.clear();
        }
    }

    pub fn budget(&self) -> usize {
        lock(&self.lru).budget
    }

    /// Bytes of decoded RGBA held in the LRU now (never above the budget).
    pub fn memory_bytes(&self) -> usize {
        lock(&self.lru).used
    }

    /// Bytes of decoded RGBA held for the stream-slot frames of the current compose
    /// (outside the budget; at most one cutscene frame in practice).
    pub fn stream_bytes(&self) -> usize {
        lock(&self.lru).stream.values().map(|f| f.bytes()).sum()
    }

    /// The pack's header (runner parameters, manifest hash, pipeline version).
    pub fn header(&self) -> &upscale::PackHeader {
        self.reader.header()
    }

    /// Payload decodes so far.
    pub fn decodes(&self) -> u64 {
        lock(&self.lru).decodes
    }

    /// Number of frame entries in the pack.
    pub fn len(&self) -> usize {
        self.reader.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reader.is_empty()
    }

    /// The blit the frames of `file` were made for.
    pub fn blit_mode(&self, file: &str) -> Option<BlitMode> {
        self.reader.blit_mode(file)
    }

    /// [`resolve`](Self::resolve) without the reason: the frame for a draw if the pack
    /// holds one that passes the same hash, blit mode and size checks.
    pub fn get(&self, req: &Request<'_>) -> Option<Arc<RgbaFrame3x>> {
        self.resolve(req).ok()
    }

    /// Resolves a draw, checking hash, blit mode and size; see [`Miss`].
    pub fn resolve(&self, req: &Request<'_>) -> Result<Arc<RgbaFrame3x>, Miss> {
        let e = self.entry(req)?;
        self.load(e, req.stream)
    }

    /// Decodes the frames of `reqs` that would resolve but are not in memory yet, in
    /// parallel (at most `available_parallelism` threads). Called before every compose
    /// with all of its draws, so a screen that shows several new full-screen frames at
    /// once pays for the slowest decode, not their sum. It also marks the frame
    /// boundary: stream frames held for the previous compose and not drawn by this one
    /// are dropped.
    pub fn prefetch(&self, reqs: &[Request<'_>]) {
        let mut todo: Vec<(&Entry, bool)> = Vec::new();
        {
            let mut lru = lock(&self.lru);
            let mut keep = HashSet::new();
            for r in reqs {
                let Ok(e) = self.entry(r) else { continue };
                if r.stream {
                    keep.insert(e.offset);
                }
                if !lru.slots.contains_key(&e.offset)
                    && !lru.stream.contains_key(&e.offset)
                    && !lru.failed.contains(&e.offset)
                    && !todo.iter().any(|(t, _)| t.offset == e.offset)
                {
                    todo.push((e, r.stream));
                }
            }
            lru.stream.retain(|off, _| keep.contains(off));
        }
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(todo.len());
        if threads <= 1 {
            for (e, stream) in todo {
                let _ = self.load(e, stream);
            }
            return;
        }
        let per = todo.len().div_ceil(threads);
        std::thread::scope(|s| {
            for chunk in todo.chunks(per) {
                s.spawn(move || {
                    for &(e, stream) in chunk {
                        let _ = self.load(e, stream);
                    }
                });
            }
        });
    }

    /// The entry for a draw, if it may be used for it.
    fn entry(&self, req: &Request<'_>) -> Result<&Entry, Miss> {
        let key = FrameKey {
            file: req.file.to_string(),
            frame: req.frame,
        };
        let e = self.reader.entry(&key).ok_or(Miss::NoEntry)?;
        if e.content_hash != req.hash {
            return Err(Miss::StaleHash);
        }
        if self.reader.blit_mode(req.file) != Some(BlitMode::from_transparent(req.transparent)) {
            return Err(Miss::BlitMode);
        }
        let want = (
            u64::from(req.src_width) * u64::from(UPSCALE),
            u64::from(req.src_height) * u64::from(UPSCALE),
        );
        if (u64::from(e.width), u64::from(e.height)) != want {
            return Err(Miss::Size);
        }
        Ok(e)
    }

    /// The decoded frame of `e`, from the LRU or decoded now (outside the lock, so
    /// [`prefetch`](Self::prefetch) threads decode in parallel).
    /// A `stream` frame goes to the per-compose stream set instead of the LRU.
    fn load(&self, e: &Entry, stream: bool) -> Result<Arc<RgbaFrame3x>, Miss> {
        {
            let mut lru = lock(&self.lru);
            if lru.failed.contains(&e.offset) {
                return Err(Miss::Decode);
            }
            lru.clock += 1;
            let now = lru.clock;
            if let Some(slot) = lru.slots.get_mut(&e.offset) {
                slot.last_use = now;
                return Ok(slot.frame.clone());
            }
            if let Some(f) = lru.stream.get(&e.offset) {
                return Ok(f.clone());
            }
        }
        let decoded = self
            .reader
            .read_payload(e)
            .and_then(|p| upscale::decode(e.codec, e.width, e.height, &p));
        let mut lru = lock(&self.lru);
        lru.decodes += 1;
        let f = match decoded {
            Ok(f) => f,
            Err(_) => {
                lru.failed.insert(e.offset);
                return Err(Miss::Decode);
            }
        };
        let opaque = f.rgba.as_chunks::<4>().0.iter().all(|p| p[3] != 0);
        let frame = Arc::new(RgbaFrame3x {
            width: f.width,
            height: f.height,
            rgba: f.rgba,
            opaque,
        });
        // Another thread may have decoded the same payload meanwhile.
        if let Some(slot) = lru.slots.get(&e.offset) {
            return Ok(slot.frame.clone());
        }
        if stream {
            if lru.stream.len() >= MAX_STREAM_HELD {
                lru.stream.clear();
            }
            lru.stream.insert(e.offset, frame.clone());
            return Ok(frame);
        }
        let size = frame.bytes();
        if size <= lru.budget {
            lru.make_room(size);
            lru.clock += 1;
            let now = lru.clock;
            lru.used += size;
            lru.slots.insert(
                e.offset,
                Slot {
                    frame: frame.clone(),
                    last_use: now,
                },
            );
        }
        Ok(frame)
    }
}

impl Lru {
    /// Evicts least recently used frames until `extra` more bytes fit the budget.
    fn make_room(&mut self, extra: usize) {
        while self.used + extra > self.budget {
            let Some((&off, _)) = self.slots.iter().min_by_key(|(_, s)| s.last_use) else {
                break;
            };
            if let Some(s) = self.slots.remove(&off) {
                self.used -= s.frame.bytes();
            }
        }
    }
}
