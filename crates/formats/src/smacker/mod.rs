//! Smacker (`SMK2`/`SMK4`) video container and decoder.
//!
//! Layout: a 104-byte header, a frame size table (`u32` per frame, the low
//! two bits are flags), a frame type table (one byte per frame: bit 0 =
//! palette chunk present, bit `1 + t` = audio chunk for track `t` present),
//! the Huffman tree block and then the frames. Inside a frame the palette
//! chunk comes first, then one length-prefixed chunk per audio track in
//! track order, and the rest is video data.

mod audio;
mod bink_audio;
mod bits;
mod huff;
mod video;

use crate::error::{FormatError, Result};
use audio::TrackDecoder;
use std::sync::Arc;
use video::{Canvas, Trees};

const HEADER_LEN: usize = 104;
/// Largest accepted width or (output) height.
const MAX_DIM: u32 = 4096;

const FLAG_RING_FRAME: u32 = 1;
const FLAG_Y_INTERLACE: u32 = 2;
const FLAG_Y_DOUBLE: u32 = 4;

const AUD_COMPRESSED: u32 = 0x8000_0000;
const AUD_PRESENT: u32 = 0x4000_0000;
const AUD_16BIT: u32 = 0x2000_0000;
const AUD_STEREO: u32 = 0x1000_0000;
const AUD_BINK_RDFT: u32 = 0x0800_0000;
const AUD_BINK_DCT: u32 = 0x0400_0000;

/// Converts the header frame rate field to microseconds per frame:
/// positive values are milliseconds, negative values are units of 10 us
/// and zero means 10 frames per second.
pub fn frame_duration_us(r: i32) -> u32 {
    let us = match r {
        r if r > 0 => i64::from(r) * 1000,
        r if r < 0 => -i64::from(r) * 10,
        _ => 100_000,
    };
    u32::try_from(us).unwrap_or(u32::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmackerHeader {
    /// 2 for `SMK2`, 4 for `SMK4`.
    pub version: u8,
    pub width: u32,
    /// Output height: the stored height, doubled when the Y-interlace or
    /// Y-double flag is set.
    pub height: u32,
    /// Number of regular frames (the ring frame, if any, is not counted).
    pub frames: u32,
    pub frame_duration_us: u32,
    pub flags: u32,
    pub audio: [Option<AudioTrackInfo>; 7],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTrackInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub compressed: bool,
    pub codec: AudioCodec,
}

/// Audio codec of a track, from the top byte of the header rate word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Uncompressed PCM (unsigned 8-bit or signed 16-bit little-endian).
    Raw,
    /// Classic Smacker Huffman/DPCM audio (flag 0x80).
    SmackerDpcm,
    /// Bink audio, RDFT variant (flag 0x08).
    BinkRdft,
}

/// Decoded audio of one track within one frame (filled by the audio decoder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioChunk {
    pub track: u8,
    pub samples: Vec<i16>,
}

/// One decoded frame, borrowed from the decoder until the next call.
pub struct FrameRef<'f> {
    pub index: u32,
    /// `width * height` palette indices, row-major.
    pub pixels: &'f [u8],
    /// 8-bit RGB palette.
    pub palette: &'f [[u8; 3]; 256],
    /// True when this frame carried a palette chunk.
    pub palette_changed: bool,
    pub audio: &'f [AudioChunk],
}

struct FrameEntry {
    offset: usize,
    size: usize,
    kind: u8,
}

/// A parsed Smacker file. Frames are decoded lazily by [`SmackerDecoder`].
pub struct Smacker {
    header: SmackerHeader,
    bytes: Vec<u8>,
    coded_height: u32,
    /// Raw audio rate words from the header (rate in the low 24 bits, codec
    /// flags in the top byte).
    audio_rates: [u32; 7],
    frames: Vec<FrameEntry>,
    trees: Trees,
}

fn truncated(what: &'static str, offset: usize) -> FormatError {
    FormatError::Truncated {
        what,
        offset: offset as u64,
    }
}

fn invalid(detail: String) -> FormatError {
    FormatError::Invalid {
        what: "smacker",
        detail,
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl Smacker {
    pub fn parse(bytes: Vec<u8>) -> Result<Smacker> {
        if bytes.len() >= 4 && !matches!(&bytes[..4], b"SMK2" | b"SMK4") {
            return Err(invalid(format!("bad signature {:?}", &bytes[..4])));
        }
        if bytes.len() < HEADER_LEN {
            return Err(truncated("smacker header", bytes.len()));
        }
        let version = bytes[3] - b'0';
        let width = u32_at(&bytes, 4);
        let coded_height = u32_at(&bytes, 8);
        let frames = u32_at(&bytes, 12);
        let rate = u32_at(&bytes, 16) as i32;
        let flags = u32_at(&bytes, 20);
        let trees_size = u32_at(&bytes, 52) as usize;
        let tree_sizes = [56, 60, 64, 68].map(|at| u32_at(&bytes, at));
        let audio_rates: [u32; 7] = std::array::from_fn(|i| u32_at(&bytes, 72 + 4 * i));

        let height = if flags & (FLAG_Y_INTERLACE | FLAG_Y_DOUBLE) != 0 {
            coded_height.saturating_mul(2)
        } else {
            coded_height
        };
        if width == 0 || coded_height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Err(invalid(format!("unsupported size {width}x{height}")));
        }

        let table_len = frames as usize + usize::from(flags & FLAG_RING_FRAME != 0);
        let sizes_at = HEADER_LEN;
        let types_at = table_len
            .checked_mul(4)
            .and_then(|n| n.checked_add(sizes_at))
            .filter(|&n| n <= bytes.len())
            .ok_or_else(|| truncated("smacker frame table", bytes.len()))?;
        let trees_at = types_at
            .checked_add(table_len)
            .filter(|&n| n <= bytes.len())
            .ok_or_else(|| truncated("smacker frame table", bytes.len()))?;
        let data_at = trees_at
            .checked_add(trees_size)
            .filter(|&n| n <= bytes.len())
            .ok_or_else(|| truncated("smacker trees", bytes.len()))?;

        let mut entries = Vec::with_capacity(table_len);
        let mut offset = data_at;
        for i in 0..table_len {
            let size = (u32_at(&bytes, sizes_at + 4 * i) & !3) as usize;
            entries.push(FrameEntry {
                offset,
                size,
                kind: bytes[types_at + i],
            });
            offset = offset.saturating_add(size);
        }

        let trees = Trees::read(&bytes[trees_at..data_at], tree_sizes)?;

        let audio = std::array::from_fn(|i| {
            let r = audio_rates[i];
            let sample_rate = r & 0x00FF_FFFF;
            (r & AUD_PRESENT != 0 || sample_rate != 0).then_some(AudioTrackInfo {
                sample_rate,
                channels: if r & AUD_STEREO != 0 { 2 } else { 1 },
                bits: if r & AUD_16BIT != 0 { 16 } else { 8 },
                compressed: r & AUD_COMPRESSED != 0,
                codec: if r & AUD_BINK_RDFT != 0 {
                    AudioCodec::BinkRdft
                } else if r & AUD_COMPRESSED != 0 {
                    AudioCodec::SmackerDpcm
                } else {
                    AudioCodec::Raw
                },
            })
        });

        for (t, (info, &r)) in audio.iter().zip(&audio_rates).enumerate() {
            let Some(info) = info else { continue };
            if r & (AUD_BINK_RDFT | AUD_BINK_DCT) == AUD_BINK_DCT {
                return Err(invalid(format!(
                    "track {t}: Bink DCT audio is not supported"
                )));
            }
            if info.codec == AudioCodec::BinkRdft && info.sample_rate == 0 {
                return Err(invalid(format!(
                    "track {t}: Bink audio with zero sample rate"
                )));
            }
        }

        Ok(Smacker {
            header: SmackerHeader {
                version,
                width,
                height,
                frames,
                frame_duration_us: frame_duration_us(rate),
                flags,
                audio,
            },
            bytes,
            coded_height,
            audio_rates,
            frames: entries,
            trees,
        })
    }

    pub fn header(&self) -> &SmackerHeader {
        &self.header
    }

    /// The raw header word for an audio track: sample rate in the low 24
    /// bits, codec flags in the top byte (0x08 / 0x04 mark Bink audio).
    pub fn audio_rate_word(&self, track: usize) -> Option<u32> {
        self.audio_rates.get(track).copied()
    }

    pub fn decoder(&self) -> SmackerDecoder<'_> {
        Self::make_decoder(SmkRef::Borrowed(self))
    }

    /// A decoder that owns the file, so it can be stored without a borrow.
    pub fn into_decoder(self) -> SmackerDecoder<'static> {
        Self::make_decoder(SmkRef::Shared(Arc::new(self)))
    }

    /// A decoder over a shared file (several decoders can read one `Smacker`).
    pub fn shared_decoder(smk: Arc<Smacker>) -> SmackerDecoder<'static> {
        Self::make_decoder(SmkRef::Shared(smk))
    }

    fn make_decoder(smk: SmkRef<'_>) -> SmackerDecoder<'_> {
        let h = &smk.header;
        let direct = h.width.is_multiple_of(4)
            && smk.coded_height.is_multiple_of(4)
            && h.height == smk.coded_height;
        let trees = smk.trees.clone();
        let canvas = Canvas::new(h.width, smk.coded_height);
        let audio_tracks = std::array::from_fn(|t| h.audio[t].as_ref().map(TrackDecoder::new));
        let out = if direct {
            Vec::new()
        } else {
            vec![0; h.width as usize * h.height as usize]
        };
        SmackerDecoder {
            smk,
            next: 0,
            trees,
            canvas,
            out,
            palette: [[0; 3]; 256],
            audio: Vec::new(),
            tracks: audio_tracks,
        }
    }
}

/// Borrowed or shared file behind a decoder.
#[derive(Clone)]
enum SmkRef<'a> {
    Borrowed(&'a Smacker),
    Shared(Arc<Smacker>),
}

impl std::ops::Deref for SmkRef<'_> {
    type Target = Smacker;
    fn deref(&self) -> &Smacker {
        match self {
            SmkRef::Borrowed(s) => s,
            SmkRef::Shared(s) => s,
        }
    }
}

/// Sequential frame decoder. Smacker frames are deltas, so decoding is
/// strictly in order; [`SmackerDecoder::rewind`] restarts from frame 0.
pub struct SmackerDecoder<'a> {
    smk: SmkRef<'a>,
    next: u32,
    trees: Trees,
    canvas: Canvas,
    /// Output buffer, used only when the canvas cannot be returned as is
    /// (size not a multiple of 4, or Y-interlace / Y-double output).
    out: Vec<u8>,
    palette: [[u8; 3]; 256],
    audio: Vec<AudioChunk>,
    tracks: [Option<TrackDecoder>; 7],
}

impl SmackerDecoder<'_> {
    /// Decodes the next regular frame, or returns `None` after the last one.
    /// The ring frame of looping files is never returned: to loop, call
    /// [`SmackerDecoder::rewind`].
    ///
    /// After an `Err` the decoder state (canvas, trees, audio overlap) is
    /// undefined and later frames would decode on top of it: the caller must
    /// call [`SmackerDecoder::rewind`] before decoding again.
    pub fn next_frame(&mut self) -> Result<Option<FrameRef<'_>>> {
        let holder = self.smk.clone();
        let smk: &Smacker = &holder;
        if self.next >= smk.header.frames {
            return Ok(None);
        }
        let index = self.next;
        let entry = &smk.frames[index as usize];
        let end = entry
            .offset
            .checked_add(entry.size)
            .filter(|&e| e <= smk.bytes.len())
            .ok_or_else(|| truncated("smacker frame", smk.bytes.len()))?;
        let mut data = &smk.bytes[entry.offset..end];
        // Advance first: a frame that fails to decode is not retried.
        self.next += 1;

        let palette_changed = entry.kind & 1 != 0;
        if palette_changed {
            let len = data.first().map_or(0, |&n| n as usize * 4);
            if len == 0 || len > data.len() {
                return Err(invalid(format!(
                    "frame {index}: bad palette chunk length {len}"
                )));
            }
            video::apply_palette(&data[1..len], &mut self.palette)?;
            data = &data[len..];
        }

        self.audio.clear();
        for track in 0..7u8 {
            if entry.kind & (2 << track) == 0 {
                continue;
            }
            if data.len() < 4 {
                return Err(truncated("smacker audio chunk", end - data.len()));
            }
            let len = u32_at(data, 0) as usize;
            if len < 4 || len > data.len() {
                return Err(invalid(format!(
                    "frame {index}: bad audio chunk length {len} for track {track}"
                )));
            }
            self.decode_audio(track, &data[4..len])?;
            data = &data[len..];
        }

        video::decode_video(
            data,
            &mut self.trees,
            &mut self.canvas,
            smk.header.version == 4,
        )?;

        let pixels: &[u8] = if self.out.is_empty() {
            &self.canvas.pixels
        } else {
            self.compose();
            &self.out
        };
        Ok(Some(FrameRef {
            index,
            pixels,
            palette: &self.palette,
            palette_changed,
            audio: &self.audio,
        }))
    }

    /// Restarts decoding at frame 0 with a black canvas and palette.
    pub fn rewind(&mut self) {
        self.next = 0;
        self.canvas.clear();
        self.out.fill(0);
        self.palette = [[0; 3]; 256];
        self.audio.clear();
        for t in self.tracks.iter_mut().flatten() {
            t.reset();
        }
    }

    /// Decodes one audio chunk (without its length prefix). Chunks for a
    /// track the header does not declare are skipped; chunks that decode to
    /// no samples (Smacker "no data" chunks) add no [`AudioChunk`].
    fn decode_audio(&mut self, track: u8, chunk: &[u8]) -> Result<()> {
        let Some(dec) = self.tracks[track as usize].as_mut() else {
            return Ok(());
        };
        let mut samples = Vec::new();
        dec.decode(chunk, &mut samples)?;
        if !samples.is_empty() {
            self.audio.push(AudioChunk { track, samples });
        }
        Ok(())
    }

    /// Crops the block-aligned canvas to the output size and applies the
    /// Y-interlace (rows on even lines, odd lines black) or Y-double (each
    /// row repeated) flags.
    fn compose(&mut self) {
        let w = self.smk.header.width as usize;
        let ch = self.smk.coded_height as usize;
        let flags = self.smk.header.flags;
        let stride = self.canvas.stride;
        for y in 0..ch {
            let src = &self.canvas.pixels[y * stride..y * stride + w];
            if flags & FLAG_Y_INTERLACE != 0 {
                self.out[2 * y * w..(2 * y + 1) * w].copy_from_slice(src);
                self.out[(2 * y + 1) * w..(2 * y + 2) * w].fill(0);
            } else if flags & FLAG_Y_DOUBLE != 0 {
                self.out[2 * y * w..(2 * y + 1) * w].copy_from_slice(src);
                self.out[(2 * y + 1) * w..(2 * y + 2) * w].copy_from_slice(src);
            } else {
                self.out[y * w..(y + 1) * w].copy_from_slice(src);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal valid file: 4x4, one frame, all trees absent.
    pub(super) fn synthetic(sig: &[u8; 4]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(sig);
        for v in [4u32, 4, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&100i32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // flags
        b.extend_from_slice(&[0; 28]); // audio sizes
        b.extend_from_slice(&1u32.to_le_bytes()); // trees size
        b.extend_from_slice(&[0; 16]); // tree alloc sizes
        b.extend_from_slice(&[0; 28]); // audio rates
        b.extend_from_slice(&[0; 4]); // pad
        assert_eq!(b.len(), 104);
        b.extend_from_slice(&0u32.to_le_bytes()); // frame 0 size
        b.push(0); // frame 0 type
        b.push(0); // trees: all four absent
        b
    }

    #[test]
    fn header_rate_conversions() {
        assert_eq!(frame_duration_us(100), 100_000);
        assert_eq!(frame_duration_us(-8333), 83_330);
        assert_eq!(frame_duration_us(0), 100_000);
    }

    #[test]
    fn header_rejects_bad_signature() {
        let r = Smacker::parse(synthetic(b"SMK3"));
        assert!(
            matches!(r, Err(FormatError::Invalid { .. })),
            "{:?}",
            r.err()
        );
    }

    #[test]
    fn header_truncated() {
        let mut b = synthetic(b"SMK2");
        b.truncate(50);
        let r = Smacker::parse(b);
        assert!(
            matches!(r, Err(FormatError::Truncated { .. })),
            "{:?}",
            r.err()
        );
    }

    #[test]
    fn synthetic_header_parses() {
        let s = Smacker::parse(synthetic(b"SMK2")).unwrap();
        let h = s.header();
        assert_eq!((h.version, h.width, h.height, h.frames), (2, 4, 4, 1));
        assert_eq!(h.frame_duration_us, 100_000);
        assert!(h.audio.iter().all(|a| a.is_none()));
        let mut d = s.decoder();
        let f = d.next_frame().unwrap().unwrap();
        assert_eq!(f.pixels, &[0; 16]);
        assert!(d.next_frame().unwrap().is_none());
    }

    fn push_bits(v: &mut Vec<u8>, value: u32, n: u32) {
        for i in 0..n {
            v.push(((value >> i) & 1) as u8);
        }
    }

    /// 6x2 coded, Y-double and ring frame flags, one 16-bit audio track,
    /// TYPE tree fixed to "SOLID colour 7, run 1".
    fn synthetic_solid() -> Vec<u8> {
        let mut bits = vec![0, 0, 0, 1, 1, 0];
        push_bits(&mut bits, 0x03, 8);
        bits.extend([0, 1, 0]);
        push_bits(&mut bits, 0x07, 8);
        bits.push(0);
        for e in [0x1111, 0x2222, 0x3333] {
            push_bits(&mut bits, e, 16);
        }
        bits.extend([0, 0]);
        let mut trees = vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            trees[i / 8] |= b << (i % 8);
        }
        // Frame: palette chunk (len 8: set entry 0, skip the other 255),
        // then an audio chunk of 8 bytes for track 0, then empty video data.
        let pal = [2u8, 0x3F, 0x00, 0x20, 0xFF, 0xFE, 0x00, 0x00];
        let audio = [8u8, 0, 0, 0, 0xAA, 0xBB, 0xCC, 0xDD];
        let frame: Vec<u8> = pal.iter().chain(&audio).copied().collect();
        let mut b = Vec::new();
        b.extend_from_slice(b"SMK4");
        for v in [6u32, 2, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&(-8333i32).to_le_bytes());
        b.extend_from_slice(&5u32.to_le_bytes()); // ring frame + Y-double
        b.extend_from_slice(&[0; 28]);
        b.extend_from_slice(&(trees.len() as u32).to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0]);
        b.extend_from_slice(&0x6000_5622u32.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&((frame.len() as u32) | 1).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // ring frame: empty
        b.extend_from_slice(&[0x03, 0x00]);
        b.extend_from_slice(&trees);
        b.extend_from_slice(&frame);
        b
    }

    #[test]
    fn owning_decoders_match_borrowing_decoder() {
        let want = {
            let s = Smacker::parse(synthetic_solid()).unwrap();
            let mut d = s.decoder();
            let f = d.next_frame().unwrap().unwrap();
            (f.pixels.to_vec(), *f.palette)
        };
        let mut owned = Smacker::parse(synthetic_solid()).unwrap().into_decoder();
        let shared = std::sync::Arc::new(Smacker::parse(synthetic_solid()).unwrap());
        let mut d2 = Smacker::shared_decoder(shared.clone());
        for d in [&mut owned, &mut d2] {
            for _ in 0..2 {
                let f = d.next_frame().unwrap().unwrap();
                assert_eq!((f.pixels.to_vec(), *f.palette), want);
                assert!(d.next_frame().unwrap().is_none());
                d.rewind();
            }
        }
        assert_eq!(shared.header().frames, 1);
    }

    #[test]
    fn synthetic_frames_decode() {
        let s = Smacker::parse(synthetic_solid()).unwrap();
        let h = s.header();
        assert_eq!((h.version, h.width, h.height, h.frames), (4, 6, 4, 1));
        assert_eq!(h.frame_duration_us, 83_330);
        assert_eq!(
            h.audio[0],
            Some(AudioTrackInfo {
                sample_rate: 22050,
                channels: 1,
                bits: 16,
                compressed: false,
                codec: AudioCodec::Raw,
            })
        );
        let mut d = s.decoder();
        for _ in 0..2 {
            let f = d.next_frame().unwrap().unwrap();
            assert_eq!(f.index, 0);
            assert!(f.palette_changed);
            assert_eq!(f.pixels, &[7; 24]);
            assert_eq!(f.palette[0], [0xFF, 0x00, 0x82]);
            let want = AudioChunk {
                track: 0,
                samples: vec![0xBBAA_u16 as i16, 0xDDCC_u16 as i16],
            };
            assert_eq!(f.audio, std::slice::from_ref(&want));
            assert!(d.next_frame().unwrap().is_none());
            d.rewind();
        }
    }

    #[test]
    fn frame_past_end_is_truncated() {
        let mut b = synthetic_solid();
        b.pop();
        let s = Smacker::parse(b).unwrap();
        let mut d = s.decoder();
        assert!(matches!(d.next_frame(), Err(FormatError::Truncated { .. })));
    }
}
