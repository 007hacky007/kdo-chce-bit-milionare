//! Smacker audio tracks: raw PCM and the classic Huffman/DPCM codec. Bink
//! audio tracks are handed to [`super::bink_audio`].
//!
//! A DPCM chunk starts with the unpacked size in bytes (`u32`), followed by
//! an LSB-first bitstream: a "data present" bit, the stereo and 16-bit flags,
//! one byte tree per output byte lane (1, 2 or 4 trees), the initial sample
//! of each channel and then one Huffman-coded delta per sample. Deltas wrap
//! around instead of clipping.

use super::bink_audio::BinkAudio;
use super::bits::BitReader;
use super::huff::ByteTree;
use super::{AudioCodec, AudioTrackInfo};
use crate::error::{FormatError, Result};

/// Largest unpacked chunk size accepted (the reference decoder's limit).
const MAX_UNPACKED: usize = 1 << 24;

fn invalid(detail: String) -> FormatError {
    FormatError::Invalid {
        what: "smacker audio",
        detail,
    }
}

/// Converts an unsigned 8-bit sample to signed 16-bit.
#[inline]
fn u8_to_i16(v: u8) -> i16 {
    (i16::from(v) - 128) << 8
}

/// Per-track decoder state.
pub(crate) enum TrackDecoder {
    Raw { bits16: bool },
    Dpcm { stereo: bool, bits16: bool },
    Bink(Box<BinkAudio>),
}

impl TrackDecoder {
    pub(crate) fn new(info: &AudioTrackInfo) -> TrackDecoder {
        let bits16 = info.bits == 16;
        match info.codec {
            AudioCodec::Raw => TrackDecoder::Raw { bits16 },
            AudioCodec::SmackerDpcm => TrackDecoder::Dpcm {
                stereo: info.channels == 2,
                bits16,
            },
            AudioCodec::BinkRdft => {
                TrackDecoder::Bink(Box::new(BinkAudio::new(info.sample_rate, info.channels)))
            }
        }
    }

    /// Decodes one chunk (without its length prefix), appending interleaved
    /// samples to `out`.
    pub(crate) fn decode(&mut self, chunk: &[u8], out: &mut Vec<i16>) -> Result<()> {
        match self {
            TrackDecoder::Raw { bits16: true } => {
                // A trailing odd byte is dropped.
                out.extend(
                    chunk
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&b| i16::from_le_bytes(b)),
                );
                Ok(())
            }
            TrackDecoder::Raw { bits16: false } => {
                out.extend(chunk.iter().map(|&v| u8_to_i16(v)));
                Ok(())
            }
            TrackDecoder::Dpcm { stereo, bits16 } => decode_dpcm(chunk, *stereo, *bits16, out),
            TrackDecoder::Bink(b) => b.decode_packet(chunk, out),
        }
    }

    /// Forgets inter-chunk state (only Bink audio has any).
    pub(crate) fn reset(&mut self) {
        if let TrackDecoder::Bink(b) = self {
            b.reset();
        }
    }
}

fn decode_dpcm(chunk: &[u8], stereo: bool, bits16: bool, out: &mut Vec<i16>) -> Result<()> {
    if chunk.len() <= 4 {
        return Err(invalid(format!("chunk too small ({} bytes)", chunk.len())));
    }
    let unpacked = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
    if unpacked > MAX_UNPACKED {
        return Err(invalid(format!("unpacked size {unpacked} too large")));
    }
    let mut br = BitReader::new(&chunk[4..]);
    if br.bit()? == 0 {
        // "No data" chunk: silence of unspecified length, nothing to output.
        return Ok(());
    }
    if (br.bit()? != 0) != stereo || (br.bit()? != 0) != bits16 {
        return Err(invalid(
            "chunk channels or sample size differ from the header".into(),
        ));
    }
    let channels = 1 + usize::from(stereo);
    let width = 1 + usize::from(bits16);
    if !unpacked.is_multiple_of(channels * width) {
        return Err(invalid(format!(
            "unpacked size {unpacked} is not a whole number of samples"
        )));
    }
    let samples = unpacked / width;

    let tree_count = 1 << (usize::from(bits16) + usize::from(stereo));
    let mut trees = Vec::with_capacity(tree_count);
    for _ in 0..tree_count {
        br.bit()?;
        trees.push(ByteTree::read(&mut br)?);
        br.bit()?;
    }

    out.reserve(samples);
    let start = out.len();
    if bits16 {
        let mut pred = [0u16; 2];
        for p in pred[..channels].iter_mut().rev() {
            *p = (br.bits(16)? as u16).swap_bytes();
        }
        out.extend(pred[..channels].iter().map(|&p| p as i16));
        for i in channels..samples {
            let ch = i & usize::from(stereo);
            let lo = trees[2 * ch].get(&mut br)?;
            let hi = trees[2 * ch + 1].get(&mut br)?;
            pred[ch] = pred[ch].wrapping_add((lo | hi << 8) as u16);
            out.push(pred[ch] as i16);
        }
    } else {
        let mut pred = [0u8; 2];
        for p in pred[..channels].iter_mut().rev() {
            *p = br.bits(8)? as u8;
        }
        out.extend(pred[..channels].iter().map(|&p| u8_to_i16(p)));
        for i in channels..samples {
            let ch = i & usize::from(stereo);
            pred[ch] = pred[ch].wrapping_add(trees[ch].get(&mut br)? as u8);
            out.push(u8_to_i16(pred[ch]));
        }
    }
    // A chunk declaring fewer samples than channels still carries the
    // initial values; keep only what was declared.
    out.truncate(start + samples);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(bits: &[u32]) -> Vec<u8> {
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            out[i / 8] |= (b as u8) << (i % 8);
        }
        out
    }

    fn push_bits(v: &mut Vec<u32>, value: u32, n: u32) {
        for i in 0..n {
            v.push((value >> i) & 1);
        }
    }

    fn chunk(unpacked: u32, bits: &[u32]) -> Vec<u8> {
        let mut c = unpacked.to_le_bytes().to_vec();
        c.extend(pack(bits));
        c
    }

    #[test]
    fn dpcm_mono_16bit() {
        // present, mono, 16-bit.
        let mut b = vec![1, 0, 1];
        // Low tree: node(leaf 0x01, leaf 0xFF); high tree: leaf 0x00.
        b.extend([0, 1, 0]);
        push_bits(&mut b, 0x01, 8);
        b.push(0);
        push_bits(&mut b, 0xFF, 8);
        b.push(0);
        b.extend([0, 0]);
        push_bits(&mut b, 0x00, 8);
        b.push(0);
        // Initial sample 0x1234, stored byte-swapped.
        push_bits(&mut b, 0x3412, 16);
        // Deltas: +1, +1, +0xFF (high byte 0).
        b.extend([0, 0, 1]);
        let mut out = Vec::new();
        decode_dpcm(&chunk(8, &b), false, true, &mut out).unwrap();
        assert_eq!(out, [0x1234, 0x1235, 0x1236, 0x1335]);
    }

    #[test]
    fn dpcm_stereo_8bit_wraps() {
        // present, stereo, 8-bit; two single-leaf trees (+0x80, +0x01).
        let mut b = vec![1, 1, 0];
        for v in [0x80, 0x01] {
            b.extend([0, 0]);
            push_bits(&mut b, v, 8);
            b.push(0);
        }
        // Initial right then left.
        push_bits(&mut b, 0xFF, 8);
        push_bits(&mut b, 0x80, 8);
        let mut out = Vec::new();
        decode_dpcm(&chunk(6, &b), true, false, &mut out).unwrap();
        // L: 0x80, 0x00, 0x80; R: 0xFF, 0x00, 0x01.
        let want = [0x80u8, 0xFF, 0x00, 0x00, 0x80, 0x01].map(u8_to_i16);
        assert_eq!(out, want);
    }

    #[test]
    fn dpcm_rejects_bad_chunks() {
        let mut out = Vec::new();
        assert!(decode_dpcm(&[0; 4], false, true, &mut out).is_err());
        assert!(decode_dpcm(&chunk(1 << 25, &[1]), false, true, &mut out).is_err());
        // Header says 16-bit, chunk says 8-bit.
        assert!(decode_dpcm(&chunk(2, &[1, 0, 0]), false, true, &mut out).is_err());
        // Odd byte count for 16-bit.
        assert!(decode_dpcm(&chunk(3, &[1, 0, 1]), false, true, &mut out).is_err());
        // "No data" chunk.
        decode_dpcm(&chunk(100, &[0]), false, true, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn raw_tracks() {
        let mut t = TrackDecoder::Raw { bits16: false };
        let mut out = Vec::new();
        t.decode(&[0x80, 0x00, 0xFF], &mut out).unwrap();
        assert_eq!(out, [0, -0x8000, 0x7F00]);
        let mut t = TrackDecoder::Raw { bits16: true };
        out.clear();
        t.decode(&[0x34, 0x12, 0xFF, 0xFF], &mut out).unwrap();
        assert_eq!(out, [0x1234, -1]);
    }
}
