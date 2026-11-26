//! Asset access: decoded Smacker overlays, streamed cutscenes and archive sounds.
//!
//! Overlays and backgrounds are decoded once into palette-indexed frames (the original
//! seeks them to arbitrary frames every tick, F6.1) and released when a screen ends.
//! Fullscreen cutscenes are never cached: [`CutsceneStream`] decodes them on demand.
//! Sounds live in `Audio1.dat` then `Audio2.dat` (F7.1-7.2) and are cached as PCM.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use formats::FormatError;
use formats::audiolib::{AudioLibIndex, HEADER_LEN, entry_bytes};
use formats::smacker::{FrameRef, Smacker, SmackerDecoder, SmackerHeader};
use formats::wav::{Pcm, parse_wav};
use gamedata::GameData;
use render::{AssetId, FrameIdent, FrameSource};
use upscale::ContentHash;

use crate::audio_types::SoundId;

/// Archives searched in order for a sound (F7.1).
const ARCHIVES: [&str; 2] = ["Audiolib/audio1.dat", "Audiolib/audio2.dat"];

/// Every asset error names the asset it concerns.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("asset not found: {name}")]
    NotFound { name: String },
    #[error("cannot read {name}: {source}")]
    Io {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot decode {name}: {source}")]
    Format {
        name: String,
        #[source]
        source: FormatError,
    },
    #[error("{name}: frame {frame} out of range 1..={frames}")]
    FrameOutOfRange {
        name: String,
        frame: u32,
        frames: u32,
    },
    #[error("{name}: audio track with sample rate 0")]
    BadSampleRate { name: String },
    #[error("asset handle {0} is not loaded")]
    BadHandle(u32),
}

impl AssetError {
    /// The asset (file, sound or handle) the error is about.
    pub fn name(&self) -> String {
        match self {
            AssetError::NotFound { name }
            | AssetError::Io { name, .. }
            | AssetError::Format { name, .. }
            | AssetError::FrameOutOfRange { name, .. }
            | AssetError::BadSampleRate { name } => name.clone(),
            AssetError::BadHandle(id) => format!("#{id}"),
        }
    }
}

fn fmt_err(name: &str) -> impl FnOnce(FormatError) -> AssetError + '_ {
    move |source| AssetError::Format {
        name: name.to_string(),
        source,
    }
}

type Palette = [[u8; 3]; 256];

#[derive(Clone)]
struct SmkFrame {
    pixels: Arc<[u8]>,
    palette: Arc<Palette>,
    /// `upscale::content_hash` of pixels and palette, computed on first use and shared by
    /// every clone (the store, the shared mirror and snapshots hash a frame once).
    hash: Arc<OnceLock<ContentHash>>,
}

impl SmkFrame {
    fn new(pixels: Arc<[u8]>, palette: Arc<Palette>) -> SmkFrame {
        SmkFrame {
            pixels,
            palette,
            hash: Arc::new(OnceLock::new()),
        }
    }

    fn hash(&self, width: u32, height: u32) -> ContentHash {
        *self
            .hash
            .get_or_init(|| upscale::content_hash(&self.pixels, &self.palette, width, height))
    }
}

#[derive(Clone)]
struct Loaded {
    name: String,
    /// The file's key in the upscale pack: lower-case path relative to `game/`.
    key: Arc<str>,
    width: u32,
    height: u32,
    frames: Vec<SmkFrame>,
    refs: u32,
    /// For a cutscene stream slot: the frame number `frames[0]` holds.
    stream: Option<u32>,
}

impl Loaded {
    /// Frame `n` by the rules of [`Assets::frame`].
    fn frame(&self, n: u32) -> Result<&SmkFrame, AssetError> {
        if let Some(held) = self.stream {
            if n == 0 || n != held {
                return Err(AssetError::FrameOutOfRange {
                    name: self.name.clone(),
                    frame: n,
                    frames: held,
                });
            }
            return Ok(&self.frames[0]);
        }
        let frames = self.frames.len() as u32;
        n.checked_sub(1)
            .and_then(|i| self.frames.get(i as usize))
            .ok_or_else(|| AssetError::FrameOutOfRange {
                name: self.name.clone(),
                frame: n,
                frames,
            })
    }

    fn view(&self, n: u32) -> Result<FrameView<'_>, AssetError> {
        let f = self.frame(n)?;
        Ok(FrameView {
            width: self.width,
            height: self.height,
            pixels: &f.pixels,
            palette: &f.palette,
            ident: Some(FrameIdent {
                file: &self.key,
                frame: n,
                hash: f.hash(self.width, self.height),
                stream: self.stream.is_some(),
            }),
        })
    }
}

/// One decoded frame: palette-indexed pixels (`width * height`) and its palette.
pub use render::FrameView;

/// A thread-safe mirror of the frames an [`Assets`] holds, for hosts that compose on
/// another thread (the SDL app). `DrawOp::Smk` references are valid only during
/// `Host::present`, so such a host calls [`SharedFrames::snapshot`] inside `present` (on
/// the game thread, where the store cannot change meanwhile) and ships the resulting
/// [`FrameSnapshot`] with the frame.
///
/// The mirror holds `Arc` clones of the decoded frames, never copies of the pixels, and
/// follows the store exactly: an entry appears when a file is loaded or a stream slot
/// opened, a stream entry changes with its held frame, and an entry disappears when the
/// store collects it. Released but uncollected handles therefore stay resolvable, like
/// in [`Assets::frame`] (the frozen frames behind the pause menu and after `End_0N`
/// rely on that). Obtained with [`Assets::share_frames`]; a store that never shares pays
/// nothing.
///
/// The snapshot also names every frame for the upscale pack, which costs a content hash
/// per decoded frame; a host without a pack turns that off with
/// [`set_hashing`](Self::set_hashing).
#[derive(Clone)]
pub struct SharedFrames(Arc<Mutex<HashMap<u32, Loaded>>>, Arc<AtomicBool>);

impl Default for SharedFrames {
    fn default() -> Self {
        SharedFrames(Arc::default(), Arc::new(AtomicBool::new(true)))
    }
}

impl SharedFrames {
    /// Whether snapshots hash frames and carry their pack identity (default on). Off,
    /// `FrameView::ident` is `None` and no frame is hashed. Takes effect from the next
    /// snapshot; every clone of the handle shares the flag.
    pub fn set_hashing(&self, on: bool) {
        self.1.store(on, Ordering::Relaxed);
    }

    pub fn hashing(&self) -> bool {
        self.1.load(Ordering::Relaxed)
    }

    /// Resolves every `DrawOp::Smk` of `frame`; references that do not resolve are left out
    /// (the compositor skips them, as it does with [`Assets`]).
    pub fn snapshot(&self, frame: &render::Frame) -> FrameSnapshot {
        let hashing = self.hashing();
        let table = lock(&self.0);
        let mut frames = HashMap::new();
        for op in &frame.ops {
            let render::DrawOp::Smk {
                asset, frame: n, ..
            } = *op
            else {
                continue;
            };
            if frames.contains_key(&(asset, n)) {
                continue;
            }
            if let Some(f) = table.get(&asset.0).and_then(|l| {
                l.frame(n).ok().map(|f| SnapFrame {
                    width: l.width,
                    height: l.height,
                    file: l.key.clone(),
                    frame: n,
                    hash: hashing.then(|| f.hash(l.width, l.height)),
                    stream: l.stream.is_some(),
                    data: f.clone(),
                })
            }) {
                frames.insert((asset, n), f);
            }
        }
        FrameSnapshot { frames }
    }
}

/// One resolved draw: the decoded frame and its identity in the upscale pack (file key,
/// 1-based frame number, content hash; hashed here on the game thread, once per frame,
/// unless hashing is off).
struct SnapFrame {
    width: u32,
    height: u32,
    file: Arc<str>,
    frame: u32,
    /// `None` when the snapshot was taken with hashing off.
    hash: Option<ContentHash>,
    /// From a cutscene stream slot.
    stream: bool,
    data: SmkFrame,
}

/// The frames one display list references, owned (`Arc`s) and `Send`.
#[derive(Default)]
pub struct FrameSnapshot {
    frames: HashMap<(AssetId, u32), SnapFrame>,
}

impl FrameSnapshot {
    /// Number of distinct (asset, frame) pairs held.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

impl FrameSource for FrameSnapshot {
    fn smk_frame(&self, asset: AssetId, frame: u32) -> Option<FrameView<'_>> {
        self.frames.get(&(asset, frame)).map(|f| FrameView {
            width: f.width,
            height: f.height,
            pixels: &f.data.pixels,
            palette: &f.data.palette,
            ident: f.hash.map(|hash| FrameIdent {
                file: &f.file,
                frame: f.frame,
                hash,
                stream: f.stream,
            }),
        })
    }
}

struct Archive {
    name: &'static str,
    path: PathBuf,
    index: AudioLibIndex,
}

/// Asset store opened on a game data directory.
pub struct Assets {
    data: GameData,
    loaded: HashMap<u32, Loaded>,
    by_name: HashMap<String, u32>,
    next_id: u32,
    /// Name of every handle ever issued (released ones included), for diagnostics.
    history: HashMap<u32, String>,
    archives: Mutex<Option<Arc<Vec<Archive>>>>,
    sounds: Mutex<HashMap<SoundId, Arc<Pcm>>>,
    /// Mirror for threaded hosts, see [`SharedFrames`].
    shared: Option<SharedFrames>,
    /// Every (pack key, transparent) blit announced with [`note_blit`](Self::note_blit).
    blits: BTreeSet<(String, bool)>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn smk_file_name(name: &str) -> String {
    if name.to_ascii_lowercase().ends_with(".smk") {
        name.to_string()
    } else {
        format!("{name}.smk")
    }
}

impl Assets {
    pub fn new(data: GameData) -> Assets {
        Assets {
            data,
            loaded: HashMap::new(),
            by_name: HashMap::new(),
            next_id: 1,
            history: HashMap::new(),
            archives: Mutex::new(None),
            sounds: Mutex::new(HashMap::new()),
            shared: None,
            blits: BTreeSet::new(),
        }
    }

    /// The upscale pack key of a game file name (`"Qs_Bg.smk"`): the lower-case path
    /// relative to `game/` of the file it resolves to, or the normalised name itself.
    fn pack_key(&self, file: &str) -> Arc<str> {
        let rel = self.data.resolve(file).and_then(|p| {
            p.strip_prefix(self.data.game_dir()).ok().map(|r| {
                r.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
        });
        Arc::from(upscale::FrameKey::new(rel.as_deref().unwrap_or(file), 1).file)
    }

    /// The upscale pack key of a handle (also after it was released).
    pub fn pack_key_of(&self, id: AssetId) -> Option<Arc<str>> {
        self.history.get(&id.0).map(|n| self.pack_key(n))
    }

    /// Records that `id` is drawn transparent or opaque (every load site calls this, so
    /// tests can check the engine's usage against `upscale::blit_mode`).
    pub fn note_blit(&mut self, id: AssetId, transparent: bool) {
        if let Some(key) = self.pack_key_of(id) {
            self.blits.insert((key.to_string(), transparent));
        }
    }

    /// Every (pack key, transparent) pair recorded by [`note_blit`](Self::note_blit).
    pub fn blit_uses(&self) -> &BTreeSet<(String, bool)> {
        &self.blits
    }

    /// A handle to a thread-safe mirror of the decoded frames (created on first call,
    /// filled with what is loaded now and kept in step from then on), see [`SharedFrames`].
    pub fn share_frames(&mut self) -> SharedFrames {
        if let Some(s) = &self.shared {
            return s.clone();
        }
        let s = SharedFrames::default();
        *lock(&s.0) = self.loaded.clone();
        self.shared = Some(s.clone());
        s
    }

    /// Insert (or replace) a loaded entry, keeping the shared mirror in step.
    fn put(&mut self, id: u32, l: Loaded) {
        if let Some(s) = &self.shared {
            lock(&s.0).insert(id, l.clone());
        }
        self.loaded.insert(id, l);
    }

    pub fn game_data(&self) -> &GameData {
        &self.data
    }

    fn read_smk(&self, name: &str) -> Result<Smacker, AssetError> {
        let file = smk_file_name(name);
        if self.data.resolve(&file).is_none() {
            return Err(AssetError::NotFound { name: file });
        }
        let bytes = self.data.read(&file).map_err(|source| AssetError::Io {
            name: file.clone(),
            source,
        })?;
        Smacker::parse(bytes).map_err(fmt_err(&file))
    }

    /// Decodes every frame of an overlay or background (`"Qs_Bg"` or `"Qs_Bg.smk"`)
    /// and returns its handle. Loading the same file again returns the same handle
    /// and counts a reference; each [`release`](Self::release) drops one.
    pub fn smk(&mut self, name: &str) -> Result<AssetId, AssetError> {
        let key = smk_file_name(name).to_ascii_lowercase();
        if let Some(&id) = self.by_name.get(&key) {
            if let Some(l) = self.loaded.get_mut(&id) {
                l.refs += 1;
            }
            return Ok(AssetId(id));
        }
        self.collect();
        let file = smk_file_name(name);
        let smk = self.read_smk(name)?;
        let (width, height, count) = {
            let h = smk.header();
            (h.width, h.height, h.frames)
        };
        let mut dec = smk.into_decoder();
        let mut frames: Vec<SmkFrame> = Vec::with_capacity(count as usize);
        while let Some(f) = dec.next_frame().map_err(fmt_err(&file))? {
            let prev = frames.last();
            let same_palette = prev.is_some_and(|p| *p.palette == *f.palette);
            let same_pixels = prev.is_some_and(|p| *p.pixels == *f.pixels);
            let frame = match prev {
                Some(p) if same_palette && same_pixels => p.clone(),
                _ => SmkFrame::new(
                    match prev {
                        Some(p) if same_pixels => p.pixels.clone(),
                        _ => Arc::from(f.pixels),
                    },
                    match prev {
                        Some(p) if same_palette => p.palette.clone(),
                        _ => Arc::new(*f.palette),
                    },
                ),
            };
            frames.push(frame);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.by_name.insert(key, id);
        self.history.insert(id, file.clone());
        let pack_key = self.pack_key(&file);
        self.put(
            id,
            Loaded {
                name: file,
                key: pack_key,
                width,
                height,
                frames,
                refs: 1,
                stream: None,
            },
        );
        Ok(AssetId(id))
    }

    /// A slot showing one frame of a streamed cutscene at a time (filled with
    /// [`set_stream_frame`](Self::set_stream_frame), freed with [`release`](Self::release)).
    /// It keeps cutscenes out of the frame cache while letting display lists reference
    /// them like any overlay: `DrawOp::Smk { asset, frame: n, .. }` resolves only while
    /// frame `n` is the one held.
    pub fn open_stream(&mut self, name: &str, width: u32, height: u32) -> AssetId {
        let id = self.next_id;
        self.next_id += 1;
        let name = smk_file_name(name);
        self.history.insert(id, name.clone());
        let blank = SmkFrame::new(
            Arc::from(vec![0u8; width as usize * height as usize]),
            Arc::new([[0; 3]; 256]),
        );
        let key = self.pack_key(&name);
        self.put(
            id,
            Loaded {
                name,
                key,
                width,
                height,
                frames: vec![blank],
                refs: 1,
                stream: Some(0),
            },
        );
        AssetId(id)
    }

    /// Replace the frame held by a stream slot with frame `n` (1-based). `pixels` must be
    /// `width * height` bytes; a mismatching frame or a non-stream handle is an error.
    pub fn set_stream_frame(
        &mut self,
        id: AssetId,
        n: u32,
        pixels: &[u8],
        palette: &[[u8; 3]; 256],
    ) -> Result<(), AssetError> {
        let l = self
            .loaded
            .get_mut(&id.0)
            .ok_or(AssetError::BadHandle(id.0))?;
        if l.stream.is_none() || pixels.len() != l.width as usize * l.height as usize {
            return Err(AssetError::BadHandle(id.0));
        }
        let f = &mut l.frames[0];
        let palette = if *f.palette == *palette {
            f.palette.clone()
        } else {
            Arc::new(*palette)
        };
        *f = SmkFrame::new(Arc::from(pixels), palette);
        l.stream = Some(n);
        if let Some(s) = &self.shared
            && let Some(m) = lock(&s.0).get_mut(&id.0)
        {
            m.frames[0] = l.frames[0].clone();
            m.stream = l.stream;
        }
        Ok(())
    }

    /// File name a handle was issued for, also after it was released.
    pub fn asset_name(&self, id: AssetId) -> Option<&str> {
        self.history.get(&id.0).map(String::as_str)
    }

    /// Drops one reference. With the last one the asset is unlinked from its name, but
    /// its frames stay resolvable by handle until the next [`collect`](Self::collect)
    /// (run by every load of a new file). This keeps an already presented frame
    /// drawable, e.g. as the frozen background of the pause and play-again menus, which
    /// [`retain`](Self::retain) its handles before loading their own overlays.
    pub fn release(&mut self, id: AssetId) {
        let Some(l) = self.loaded.get_mut(&id.0) else {
            return;
        };
        if l.refs == 0 {
            return;
        }
        l.refs -= 1;
        if l.refs == 0 {
            self.by_name.retain(|_, v| *v != id.0);
        }
    }

    /// Adds a reference to a handle that is still resolvable (loaded, or released but
    /// not yet collected). Returns `false` if it is gone.
    pub fn retain(&mut self, id: AssetId) -> bool {
        match self.loaded.get_mut(&id.0) {
            Some(l) => {
                l.refs += 1;
                true
            }
            None => false,
        }
    }

    /// Frees every released asset.
    pub fn collect(&mut self) {
        self.loaded.retain(|_, l| l.refs > 0);
        if let Some(s) = &self.shared {
            lock(&s.0).retain(|id, _| self.loaded.contains_key(id));
        }
    }

    /// Handles with at least one reference.
    pub fn live_handles(&self) -> usize {
        self.loaded.values().filter(|l| l.refs > 0).count()
    }

    fn get(&self, id: AssetId) -> Result<&Loaded, AssetError> {
        self.loaded.get(&id.0).ok_or(AssetError::BadHandle(id.0))
    }

    /// `(width, height, frame count)` of a loaded asset.
    pub fn info(&self, id: AssetId) -> Result<(u32, u32, u32), AssetError> {
        let l = self.get(id)?;
        Ok((l.width, l.height, l.frames.len() as u32))
    }

    /// Frame `n`, 1-based as in the original's animation tables; 0 and
    /// `n > frames` are errors.
    pub fn frame(&self, id: AssetId, n: u32) -> Result<FrameView<'_>, AssetError> {
        self.get(id)?.view(n)
    }

    /// Bytes held by decoded frames (shared frames and palettes counted once).
    pub fn memory_bytes(&self) -> usize {
        let mut seen: Vec<*const u8> = Vec::new();
        let mut total = 0;
        for l in self.loaded.values() {
            for f in &l.frames {
                let p = f.pixels.as_ptr();
                if !seen.contains(&p) {
                    seen.push(p);
                    total += f.pixels.len();
                }
                let q = Arc::as_ptr(&f.palette).cast::<u8>();
                if !seen.contains(&q) {
                    seen.push(q);
                    total += size_of::<Palette>();
                }
            }
        }
        total
    }

    /// Opens a fullscreen cutscene for sequential, uncached decoding.
    pub fn cutscene(&self, name: &str) -> Result<CutsceneStream, AssetError> {
        let file = smk_file_name(name);
        let smk = self.read_smk(name)?;
        if smk
            .header()
            .audio
            .iter()
            .flatten()
            .any(|a| a.sample_rate == 0)
        {
            return Err(AssetError::BadSampleRate { name: file });
        }
        let header = smk.header().clone();
        Ok(CutsceneStream {
            name: file,
            header,
            decoder: smk.into_decoder(),
        })
    }

    fn archives(&self) -> Result<Arc<Vec<Archive>>, AssetError> {
        let mut slot = lock(&self.archives);
        if let Some(a) = slot.as_ref() {
            return Ok(a.clone());
        }
        let mut list = Vec::new();
        for name in ARCHIVES {
            let path = self
                .data
                .resolve(name)
                .ok_or_else(|| AssetError::NotFound {
                    name: name.to_string(),
                })?;
            let io_err = |source| AssetError::Io {
                name: name.to_string(),
                source,
            };
            let mut head = vec![0u8; HEADER_LEN];
            File::open(&path)
                .and_then(|mut f| f.read_exact(&mut head))
                .map_err(io_err)?;
            let index = AudioLibIndex::parse(&head).map_err(fmt_err(name))?;
            list.push(Archive { name, path, index });
        }
        let list = Arc::new(list);
        *slot = Some(list.clone());
        Ok(list)
    }

    /// Resolves a sound in `Audio1.dat` then `Audio2.dat` and caches the decoded PCM.
    pub fn sound(&self, id: SoundId) -> Result<Arc<Pcm>, AssetError> {
        if let Some(p) = lock(&self.sounds).get(&id) {
            return Ok(p.clone());
        }
        let label = id.to_string();
        let entry_name = format!("{label}.wav");
        let archives = self.archives()?;
        let (arc, entry) = archives
            .iter()
            .find_map(|a| a.index.find(&entry_name).map(|e| (a, e)))
            .ok_or_else(|| AssetError::NotFound {
                name: label.clone(),
            })?;
        let io_err = |source| AssetError::Io {
            name: format!("{label} in {}", arc.name),
            source,
        };
        let mut f = File::open(&arc.path).map_err(io_err)?;
        let file_len = f.metadata().map_err(io_err)?.len();
        if u64::from(entry.offset) + u64::from(entry.size) > file_len {
            return Err(io_err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "entry at {}+{} lies beyond the archive end ({file_len} bytes)",
                    entry.offset, entry.size
                ),
            )));
        }
        let mut data = vec![0u8; entry.size as usize];
        f.seek(SeekFrom::Start(u64::from(entry.offset)))
            .and_then(|_| f.read_exact(&mut data))
            .map_err(io_err)?;
        let whole_entry = formats::audiolib::AudioEntry {
            name: entry.name.clone(),
            offset: 0,
            size: entry.size,
        };
        let bytes = entry_bytes(&data, &whole_entry).map_err(fmt_err(&label))?;
        let pcm = Arc::new(parse_wav(bytes).map_err(fmt_err(&label))?);
        lock(&self.sounds).insert(id, pcm.clone());
        Ok(pcm)
    }
}

/// Sequential cutscene decoder. After an error call [`rewind`](Self::rewind).
pub struct CutsceneStream {
    name: String,
    header: SmackerHeader,
    decoder: SmackerDecoder<'static>,
}

impl CutsceneStream {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn header(&self) -> &SmackerHeader {
        &self.header
    }

    /// Next frame with its audio chunks, or `None` after the last one.
    pub fn next_frame(&mut self) -> Result<Option<FrameRef<'_>>, AssetError> {
        let name = &self.name;
        self.decoder.next_frame().map_err(fmt_err(name))
    }

    pub fn rewind(&mut self) {
        self.decoder.rewind();
    }
}

impl FrameSource for Assets {
    fn smk_frame(&self, asset: AssetId, frame: u32) -> Option<FrameView<'_>> {
        self.frame(asset, frame).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_types::Prefix;
    use std::fs;
    use std::path::Path;

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join("game").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn wav(rate: u32) -> Vec<u8> {
        let data = [1u8, 0, 2, 0];
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    fn archive(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        let mut body = Vec::new();
        for (i, (name, data)) in entries.iter().enumerate() {
            let (stem, ext) = name.split_once('.').unwrap();
            let at = 4 + i * 32;
            h[at..at + stem.len()].copy_from_slice(stem.as_bytes());
            let ext_at = at + stem.len() + 1;
            h[ext_at..ext_at + ext.len()].copy_from_slice(ext.as_bytes());
            body.extend_from_slice(data);
        }
        // offsets and sizes tables
        let mut off = HEADER_LEN as u32;
        for (i, (_, data)) in entries.iter().enumerate() {
            h[OFFSETS + i * 4..OFFSETS + i * 4 + 4].copy_from_slice(&off.to_le_bytes());
            h[SIZES + i * 4..SIZES + i * 4 + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
            off += data.len() as u32;
        }
        h.extend_from_slice(&body);
        h
    }

    // Offset and size table positions in the audiolib header.
    const OFFSETS: usize = 0x8004;
    const SIZES: usize = 0x9004;

    #[test]
    fn entry_beyond_the_archive_is_an_error_not_an_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = archive(&[("Sfex0001.wav", wav(22050))]);
        bytes[SIZES..SIZES + 4].copy_from_slice(&0xF000_0000u32.to_le_bytes());
        write(dir.path(), "Audiolib/audio1.dat", &bytes);
        write(dir.path(), "Audiolib/audio2.dat", &archive(&[]));
        let a = Assets::new(GameData::open(dir.path()).unwrap());
        let e = a.sound(SoundId::new(Prefix::Sfex, 1)).unwrap_err();
        assert!(matches!(e, AssetError::Io { .. }), "{e}");
        assert!(e.to_string().contains("SFEX0001"), "{e}");
        assert!(e.to_string().contains("beyond"), "{e}");
    }

    #[test]
    fn missing_audio1_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Audiolib/audio2.dat", &archive(&[]));
        let a = Assets::new(GameData::open(dir.path()).unwrap());
        let e = a.sound(SoundId::new(Prefix::Sfex, 1)).unwrap_err();
        assert!(matches!(e, AssetError::NotFound { .. }), "{e}");
        assert!(e.to_string().contains("audio1.dat"), "{e}");
    }

    #[test]
    fn synthetic_archives_resolve_in_order_and_cache() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Audiolib/audio1.dat",
            &archive(&[("Sfex0001.wav", wav(22050))]),
        );
        write(
            dir.path(),
            "Audiolib/audio2.dat",
            &archive(&[("Char0002q.wav", wav(11025)), ("Sfex0001.wav", wav(8000))]),
        );
        let a = Assets::new(GameData::open(dir.path()).unwrap());
        let s = a.sound(SoundId::new(Prefix::Sfex, 1)).unwrap();
        assert_eq!(s.sample_rate, 22050);
        let q = a.sound(SoundId::with_suffix(Prefix::Char, 2, 'q')).unwrap();
        assert_eq!((q.sample_rate, q.samples.len()), (11025, 2));
        assert!(Arc::ptr_eq(
            &s,
            &a.sound(SoundId::new(Prefix::Sfex, 1)).unwrap()
        ));
        let e = a.sound(SoundId::new(Prefix::Musi, 9)).unwrap_err();
        assert!(
            matches!(&e, AssetError::NotFound { name } if name == "MUSI0009"),
            "{e}"
        );
    }

    fn smk_op(asset: AssetId, frame: u32) -> render::DrawOp {
        render::DrawOp::Smk {
            asset,
            frame,
            x: 0,
            y: 0,
            transparent: false,
        }
    }

    #[test]
    fn shared_frames_snapshot_follows_the_store() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.txt", b"x");
        let mut a = Assets::new(GameData::open(dir.path()).unwrap());
        let mut pal = [[0u8; 3]; 256];
        pal[1] = [9, 8, 7];
        // Loaded before the mirror exists: captured when it is created.
        let early = a.open_stream("Early", 1, 1);
        a.set_stream_frame(early, 1, &[7], &pal).unwrap();
        let shared = a.share_frames();
        let id = a.open_stream("Cut", 2, 1);
        a.set_stream_frame(id, 5, &[1, 2], &pal).unwrap();
        let mut frame = render::Frame::new();
        frame.push(smk_op(id, 5));
        frame.push(smk_op(id, 4));
        frame.push(smk_op(early, 0));
        frame.push(smk_op(AssetId(99), 1));
        let snap = shared.snapshot(&frame);
        let f = snap.smk_frame(id, 5).unwrap();
        assert_eq!((f.width, f.height, f.pixels), (2, 1, &[1u8, 2][..]));
        assert_eq!(f.palette[1], [9, 8, 7]);
        assert!(snap.smk_frame(id, 4).is_none());
        assert!(snap.smk_frame(early, 0).is_none());
        assert!(snap.smk_frame(AssetId(99), 1).is_none());
        // The snapshot owns its data: later stream frames do not change it.
        a.set_stream_frame(id, 6, &[3, 4], &pal).unwrap();
        assert_eq!(snap.smk_frame(id, 5).unwrap().pixels, &[1u8, 2][..]);
        let mut next = render::Frame::new();
        next.push(smk_op(id, 6));
        next.push(smk_op(early, 1));
        // Released but not yet collected handles still resolve, as in `Assets::frame`.
        a.release(id);
        let snap = shared.snapshot(&next);
        assert_eq!(snap.smk_frame(id, 6).unwrap().pixels, &[3u8, 4][..]);
        assert!(snap.smk_frame(early, 1).is_some());
        a.collect();
        let snap = shared.snapshot(&next);
        assert!(snap.smk_frame(id, 6).is_none());
        assert!(snap.smk_frame(early, 1).is_some());
        assert_send::<FrameSnapshot>();
        assert_send::<SharedFrames>();
    }

    /// Snapshots name each frame for the upscale pack: file key, 1-based frame and the
    /// content hash, hashed once per decoded frame and shared with the store.
    #[test]
    fn snapshot_carries_pack_identity() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Cuts/Mm_Open.smk", b"not parsed");
        let mut a = Assets::new(GameData::open(dir.path()).unwrap());
        let shared = a.share_frames();
        let mut pal = [[0u8; 3]; 256];
        pal[2] = [1, 2, 3];
        let id = a.open_stream("Mm_Open", 2, 1);
        let unresolved = a.open_stream("Nowhere/X.SMK", 1, 1);
        a.set_stream_frame(id, 5, &[1, 2], &pal).unwrap();
        a.set_stream_frame(unresolved, 1, &[2], &pal).unwrap();
        let mut frame = render::Frame::new();
        frame.push(smk_op(id, 5));
        frame.push(smk_op(unresolved, 1));
        let snap = shared.snapshot(&frame);
        let want = upscale::content_hash(&[1, 2], &pal, 2, 1);
        let ident = snap.smk_frame(id, 5).unwrap().ident.unwrap();
        assert_eq!(
            ident,
            FrameIdent {
                file: "cuts/mm_open.smk",
                frame: 5,
                hash: want,
                stream: true,
            }
        );
        // A name that does not resolve keeps its normalised spelling.
        assert_eq!(
            snap.smk_frame(unresolved, 1).unwrap().ident.unwrap().file,
            "nowhere/x.smk"
        );
        // The store saw the same hash computation (one OnceLock shared by both).
        assert_eq!(a.loaded[&id.0].frames[0].hash.get(), Some(&want));
        assert_eq!(a.frame(id, 5).unwrap().ident.unwrap().hash, want);
        // The next stream frame is a new frame with its own hash.
        a.set_stream_frame(id, 6, &[2, 1], &pal).unwrap();
        assert!(a.loaded[&id.0].frames[0].hash.get().is_none());
        let mut next = render::Frame::new();
        next.push(smk_op(id, 6));
        let snap = shared.snapshot(&next);
        let i6 = snap.smk_frame(id, 6).unwrap().ident.unwrap();
        assert_eq!(
            (i6.frame, i6.hash),
            (6, upscale::content_hash(&[2, 1], &pal, 2, 1))
        );
        assert_eq!(a.pack_key_of(id).as_deref(), Some("cuts/mm_open.smk"));
        a.note_blit(id, false);
        assert!(
            a.blit_uses()
                .contains(&("cuts/mm_open.smk".to_string(), false))
        );
    }

    /// Without an upscaled pack the app turns hashing off: snapshots carry no identity
    /// and nothing is hashed; turning it back on hashes from the next snapshot on.
    #[test]
    fn snapshot_skips_hashing_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.txt", b"x");
        let mut a = Assets::new(GameData::open(dir.path()).unwrap());
        let shared = a.share_frames();
        assert!(shared.hashing());
        shared.set_hashing(false);
        let pal = [[0u8; 3]; 256];
        let id = a.open_stream("Cut", 2, 1);
        a.set_stream_frame(id, 1, &[1, 2], &pal).unwrap();
        let mut frame = render::Frame::new();
        frame.push(smk_op(id, 1));
        let snap = shared.snapshot(&frame);
        let f = snap.smk_frame(id, 1).unwrap();
        assert_eq!(f.pixels, &[1u8, 2][..]);
        assert!(f.ident.is_none());
        assert!(a.loaded[&id.0].frames[0].hash.get().is_none(), "not hashed");
        shared.set_hashing(true);
        let snap = shared.snapshot(&frame);
        assert_eq!(
            snap.smk_frame(id, 1).unwrap().ident.unwrap().hash,
            upscale::content_hash(&[1, 2], &pal, 2, 1)
        );
    }

    fn assert_send<T: Send + 'static>() {}

    #[test]
    fn missing_smk_and_bad_handle_name_the_asset() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.txt", b"x");
        let mut a = Assets::new(GameData::open(dir.path()).unwrap());
        let e = a.smk("Nope").unwrap_err();
        assert!(
            matches!(&e, AssetError::NotFound { name } if name == "Nope.smk"),
            "{e}"
        );
        assert!(matches!(
            a.frame(AssetId(99), 1),
            Err(AssetError::BadHandle(99))
        ));
        assert!(a.cutscene("Nope").is_err());
    }

    fn corpus() -> Option<Assets> {
        let Ok(root) = std::env::var("MILIONAR_DATA") else {
            println!("skipped: MILIONAR_DATA not set");
            return None;
        };
        Some(Assets::new(GameData::open(Path::new(&root)).unwrap()))
    }

    #[test]
    fn corpus_qs_bg_frames() {
        let Some(mut a) = corpus() else { return };
        let id = a.smk("Qs_Bg").unwrap();
        assert_eq!(a.info(id).unwrap(), (640, 480, 37));
        let f = a.frame(id, 37).unwrap();
        assert_eq!((f.width, f.height, f.pixels.len()), (640, 480, 640 * 480));
        assert!(a.frame(id, 1).is_ok());
        let e = a.frame(id, 38).unwrap_err();
        assert!(
            matches!(
                e,
                AssetError::FrameOutOfRange {
                    frame: 38,
                    frames: 37,
                    ..
                }
            ),
            "{e}"
        );
        assert!(e.to_string().contains("Qs_Bg.smk"));
        assert!(a.frame(id, 0).is_err());
        let shared = a.share_frames();
        let mut frame = render::Frame::new();
        frame.push(smk_op(id, 37));
        frame.push(smk_op(id, 38));
        let snap = shared.snapshot(&frame);
        assert_eq!(
            snap.smk_frame(id, 37).unwrap().pixels,
            a.frame(id, 37).unwrap().pixels
        );
        assert!(snap.smk_frame(id, 38).is_none());
        let ident = snap.smk_frame(id, 37).unwrap().ident.unwrap();
        assert_eq!((ident.file, ident.frame), ("quesansw/qs_bg.smk", 37));
        let f37 = a.frame(id, 37).unwrap();
        assert_eq!(
            ident.hash,
            upscale::content_hash(f37.pixels, f37.palette, 640, 480)
        );
        // same file again shares the handle; release is reference counted
        assert_eq!(a.smk("qs_bg.smk").unwrap(), id);
        a.release(id);
        assert!(a.frame(id, 1).is_ok());
        a.release(id);
        // Released but not yet collected: still drawable, no longer found by name.
        assert!(a.frame(id, 1).is_ok());
        assert_eq!(a.live_handles(), 0);
        assert!(a.retain(id));
        a.release(id);
        a.collect();
        assert!(matches!(a.frame(id, 1), Err(AssetError::BadHandle(_))));
        assert!(!a.retain(id));
    }

    #[test]
    fn corpus_question_screen_set_memory() {
        let Some(mut a) = corpus() else { return };
        let ids: Vec<_> = [
            "Mm_Bg", "Qs_Bg", "Mm_Ib1", "Mm_Ib2", "Mm_Ib3", "Mm_Ib4", "Mm_Heads", "Qs_PnBox",
            "Mm_Regis", "Qs_Abcs",
        ]
        .iter()
        .map(|n| a.smk(n).unwrap())
        .collect();
        println!("question-screen set: {} bytes decoded", a.memory_bytes());
        for id in ids {
            a.release(id);
        }
        a.collect();
        assert_eq!(a.memory_bytes(), 0);
    }

    #[test]
    fn corpus_sounds_resolve() {
        let Some(a) = corpus() else { return };
        let q = a
            .sound(SoundId::with_suffix(Prefix::Char, 1103, 'q'))
            .unwrap();
        assert!(q.sample_rate > 0 && !q.samples.is_empty());
        a.sound(SoundId::new(Prefix::Sfex, 4057)).unwrap();
        let e = a.sound(SoundId::new(Prefix::Sfex, 9999)).unwrap_err();
        assert!(
            matches!(&e, AssetError::NotFound { name } if name == "SFEX9999"),
            "{e}"
        );
    }

    #[test]
    fn corpus_cutscene_streams_with_audio() {
        let Some(a) = corpus() else { return };
        let mut c = a.cutscene("Mm_open").unwrap();
        let (w, h) = (c.header().width as usize, c.header().height as usize);
        let mut n = 0;
        let mut audio = 0;
        while let Some(f) = c.next_frame().unwrap() {
            assert_eq!(f.pixels.len(), w * h);
            audio += f.audio.len();
            n += 1;
        }
        assert_eq!(n, c.header().frames);
        assert!(audio > 0);
    }
}
