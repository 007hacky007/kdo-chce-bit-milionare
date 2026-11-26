//! Minimal RIFF WAVE (PCM) decoder.

use crate::{FormatError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    /// Bit depth of the source data (8 or 16); `samples` is always widened to i16.
    pub bits_per_sample: u16,
    /// Interleaved samples.
    pub samples: Vec<i16>,
}

fn invalid(detail: &str) -> FormatError {
    FormatError::Invalid {
        what: "wav",
        detail: detail.to_string(),
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Decodes PCM (format 1) 8/16-bit, mono/stereo WAVE data. 8-bit input is
/// converted with `(s - 128) << 8`.
pub fn parse_wav(bytes: &[u8]) -> Result<Pcm> {
    if bytes.len() < 12 {
        return Err(FormatError::Truncated {
            what: "wav header",
            offset: bytes.len() as u64,
        });
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(invalid("not a RIFF WAVE file"));
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u32, u16, u16)> = None; // rate, channels, bits
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(bytes, pos + 4) as usize;
        let body = pos + 8;
        let end = body + size;
        if id == b"fmt " {
            if size < 16 || end > bytes.len() {
                return Err(FormatError::Truncated {
                    what: "wav fmt chunk",
                    offset: bytes.len() as u64,
                });
            }
            let tag = u16_at(bytes, body);
            let channels = u16_at(bytes, body + 2);
            let rate = u32_at(bytes, body + 4);
            let bits = u16_at(bytes, body + 14);
            if tag != 1 {
                return Err(invalid(&format!("unsupported format tag {tag}")));
            }
            if !(1..=2).contains(&channels) {
                return Err(invalid(&format!("unsupported channel count {channels}")));
            }
            if bits != 8 && bits != 16 {
                return Err(invalid(&format!("unsupported bit depth {bits}")));
            }
            if rate == 0 {
                return Err(invalid("sample rate 0"));
            }
            fmt = Some((rate, channels, bits));
        } else if id == b"data" {
            let (sample_rate, channels, bits) =
                fmt.ok_or_else(|| invalid("data chunk before fmt chunk"))?;
            if end > bytes.len() {
                return Err(FormatError::Truncated {
                    what: "wav data chunk",
                    offset: bytes.len() as u64,
                });
            }
            let data = &bytes[body..end];
            let samples = if bits == 8 {
                data.iter().map(|&s| (s as i16 - 128) << 8).collect()
            } else {
                data.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_le_bytes(*c))
                    .collect()
            };
            return Ok(Pcm {
                sample_rate,
                channels,
                bits_per_sample: bits,
                samples,
            });
        }
        pos = end + (size & 1);
    }
    Err(invalid("no data chunk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(bits: u16, channels: u16, extra: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&22050u32.to_le_bytes());
        let align = channels * bits / 8;
        out.extend_from_slice(&(22050 * align as u32).to_le_bytes());
        out.extend_from_slice(&align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(extra);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        let riff = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff.to_le_bytes());
        out
    }

    #[test]
    fn wav_16bit_mono() {
        let data: Vec<u8> = [1i16, -2, 300, -32768]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let pcm = parse_wav(&wav(16, 1, &[], &data)).unwrap();
        assert_eq!(pcm.sample_rate, 22050);
        assert_eq!(pcm.channels, 1);
        assert_eq!(pcm.samples, vec![1, -2, 300, -32768]);
    }

    #[test]
    fn wav_8bit_converts_to_i16() {
        let pcm = parse_wav(&wav(8, 1, &[], &[0, 128, 255])).unwrap();
        assert_eq!(pcm.samples, vec![-32768, 0, 32512]);
    }

    #[test]
    fn wav_skips_list_chunk() {
        let mut extra = b"LIST".to_vec();
        extra.extend_from_slice(&3u32.to_le_bytes());
        extra.extend_from_slice(&[9, 9, 9, 0]); // 3 bytes + pad
        let pcm = parse_wav(&wav(16, 2, &extra, &[1, 0, 2, 0])).unwrap();
        assert_eq!(pcm.channels, 2);
        assert_eq!(pcm.samples, vec![1, 2]);
    }

    #[test]
    fn wav_truncated_data_errors() {
        let mut w = wav(16, 1, &[], &[1, 0, 2, 0]);
        w.truncate(w.len() - 2);
        assert!(matches!(parse_wav(&w), Err(FormatError::Truncated { .. })));
    }

    #[test]
    fn wav_rejects_garbage() {
        assert!(parse_wav(b"nope").is_err());
        assert!(parse_wav(&[]).is_err());
        let mut w = wav(16, 1, &[], &[1, 0]);
        w[20] = 3; // float format
        assert!(matches!(parse_wav(&w), Err(FormatError::Invalid { .. })));
    }

    #[test]
    fn wav_rejects_zero_sample_rate() {
        let mut w = wav(16, 1, &[], &[1, 0]);
        w[24..28].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            parse_wav(&w),
            Err(FormatError::Invalid { detail, .. }) if detail.contains("sample rate")
        ));
    }
}
