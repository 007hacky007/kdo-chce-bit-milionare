//! Bink audio, RDFT variant, as embedded in Smacker files (header audio
//! flag 0x08). Derived from FFmpeg `libavcodec/binkaudio.c` and the RDFT in
//! `libavutil/tx_template.c`, see THIRD_PARTY_NOTICES.md.
//!
//! In this variant the channels are interleaved before the transform, so a
//! stereo track is coded as one mono stream at twice the sample rate. Each
//! block holds quantised frequency coefficients for `frame_len` output
//! samples; the last `frame_len / 16` samples are cross-faded into the start
//! of the next block, so every block yields `frame_len - frame_len / 16`
//! samples. A Smacker audio chunk is one packet: a `u32` decoded size
//! (ignored), then blocks, each padded to a 32-bit boundary.

use super::bits::BitReader;
use crate::error::{FormatError, Result};

/// Band edges in Hz (the WMA critical frequencies).
const CRITICAL_FREQS: [u32; 25] = [
    100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000, 2320, 2700, 3150, 3700,
    4400, 5300, 6400, 7700, 9500, 12000, 15500, 24500,
];

/// `0.066399999 / log10(e)`, the quantiser step on a natural-log scale.
const QUANT_STEP: f32 = 0.152_891_647_872_219_54_f64 as f32;

const RLE_LENGTHS: [usize; 16] = [2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 32, 64];

fn truncated(br: &BitReader<'_>) -> FormatError {
    FormatError::Truncated {
        what: "bink audio block",
        offset: (br.position() / 8) as u64,
    }
}

pub(crate) struct BinkAudio {
    frame_len: usize,
    overlap_len: usize,
    num_bands: usize,
    root: f32,
    bands: [usize; 26],
    quant_table: [f32; 96],
    previous: Vec<f32>,
    first: bool,
    /// Spectrum, `frame_len + 2` floats (`frame_len / 2 + 1` complex values).
    coeffs: Vec<f32>,
    /// Time-domain output of one block.
    block: Vec<f32>,
    fft: Fft,
}

impl BinkAudio {
    /// `channels` is 1 or 2 (validated by the header parser); the sample
    /// rate must be nonzero.
    pub(crate) fn new(sample_rate: u32, channels: u16) -> BinkAudio {
        let mut frame_len_bits = match sample_rate {
            r if r < 22050 => 9,
            r if r < 44100 => 10,
            _ => 11,
        };
        let channels = u32::from(channels.clamp(1, 2));
        let rate = sample_rate.max(1).saturating_mul(channels);
        if channels == 2 {
            frame_len_bits += 1;
        }
        let frame_len = 1usize << frame_len_bits;
        let overlap_len = frame_len / 16;
        let rate_half = rate.div_ceil(2) as usize;
        let root = (2.0 / ((frame_len as f64).sqrt() * 32768.0)) as f32;
        let quant_table: [f32; 96] = std::array::from_fn(|i| (i as f32 * QUANT_STEP).exp() * root);

        let mut num_bands = 1;
        while num_bands < 25 && rate_half > CRITICAL_FREQS[num_bands - 1] as usize {
            num_bands += 1;
        }
        let mut bands = [0usize; 26];
        bands[0] = 2;
        for i in 1..num_bands {
            bands[i] = (CRITICAL_FREQS[i - 1] as usize * frame_len / rate_half) & !1;
        }
        bands[num_bands] = frame_len;

        BinkAudio {
            frame_len,
            overlap_len,
            num_bands,
            root,
            bands,
            quant_table,
            previous: vec![0.0; overlap_len],
            first: true,
            coeffs: vec![0.0; frame_len + 2],
            block: vec![0.0; frame_len],
            fft: Fft::new(frame_len / 2),
        }
    }

    /// Interleaved samples produced per block.
    #[cfg(test)]
    pub(crate) fn block_size(&self) -> usize {
        self.frame_len - self.overlap_len
    }

    pub(crate) fn reset(&mut self) {
        self.first = true;
    }

    /// Decodes one Smacker audio chunk, appending `i16` samples to `out`.
    pub(crate) fn decode_packet(&mut self, chunk: &[u8], out: &mut Vec<i16>) -> Result<()> {
        if chunk.len() < 4 {
            return Err(FormatError::Invalid {
                what: "bink audio",
                detail: format!("packet too small ({} bytes)", chunk.len()),
            });
        }
        let mut br = BitReader::new(&chunk[4..]);
        while br.bits_left() > 0 {
            // An error drops the blocks already decoded from this chunk on purpose: next_frame's Err requires rewind().
            self.decode_block(&mut br)?;
            let pad = br.position().wrapping_neg() & 31;
            br.skip(pad)?;
            let n = self.frame_len - self.overlap_len;
            out.extend(self.block[..n].iter().map(|&v| to_i16(v)));
        }
        Ok(())
    }

    fn get_float(br: &mut BitReader<'_>) -> Result<f32> {
        let power = br.bits(5)? as i32;
        let mantissa = br.bits(23)? as f32;
        // 2^(power - 23) is a normal f32 for power in 0..32.
        let scale = f32::from_bits(((power - 23 + 127) as u32) << 23);
        let f = mantissa * scale;
        Ok(if br.bit()? != 0 { -f } else { f })
    }

    fn decode_block(&mut self, br: &mut BitReader<'_>) -> Result<()> {
        let frame_len = self.frame_len;
        if br.bits_left() < 58 {
            return Err(truncated(br));
        }
        let c = &mut self.coeffs;
        c[0] = Self::get_float(br)? * self.root;
        c[1] = Self::get_float(br)? * self.root;

        let mut quant = [0f32; 25];
        for q in &mut quant[..self.num_bands] {
            *q = self.quant_table[(br.bits(8)? as usize).min(95)];
        }

        let bands = &self.bands;
        let mut k = 0usize;
        let mut q = quant[0];
        let mut i = 2usize;
        while i < frame_len {
            let j = if br.bit()? != 0 {
                i + RLE_LENGTHS[br.bits(4)? as usize] * 8
            } else {
                i + 8
            }
            .min(frame_len);
            let width = br.bits(4)?;
            if width == 0 {
                c[i..j].fill(0.0);
                i = j;
                while bands[k] < i {
                    q = quant[k];
                    k += 1;
                }
            } else {
                while i < j {
                    if bands[k] == i {
                        q = quant[k];
                        k += 1;
                    }
                    let coeff = br.bits(width)?;
                    c[i] = if coeff == 0 {
                        0.0
                    } else if br.bit()? != 0 {
                        -q * coeff as f32
                    } else {
                        q * coeff as f32
                    };
                    i += 1;
                }
            }
        }

        // Conjugate (the transform runs the other way) and move the Nyquist
        // term, stored in slot 1, to the end.
        for i in (2..frame_len).step_by(2) {
            c[i + 1] = -c[i + 1];
        }
        c[frame_len] = c[1];
        c[frame_len + 1] = 0.0;
        c[1] = 0.0;
        inverse_rdft(&self.fft, c, &mut self.block);

        let ov = self.overlap_len;
        if !self.first {
            let count = ov as f32;
            for i in 0..ov {
                self.block[i] =
                    (self.previous[i] * (ov - i) as f32 + self.block[i] * i as f32) / count;
            }
        }
        self.previous
            .copy_from_slice(&self.block[frame_len - ov..frame_len]);
        self.first = false;
        Ok(())
    }
}

/// Float to `i16` with round-half-even and clamping (as swresample does).
#[inline]
fn to_i16(v: f32) -> i16 {
    (v * 32768.0).round_ties_even().clamp(-32768.0, 32767.0) as i16
}

/// Complex-to-real inverse DFT of length `2 * fft.n` with scale 0.5, using
/// the half-length complex FFT and the twiddle post-processing of FFmpeg's
/// `rdft_c2r`. `spec` holds `n + 1` complex values (re, im pairs) and is
/// overwritten.
fn inverse_rdft(fft: &Fft, spec: &mut [f32], out: &mut [f32]) {
    let len2 = fft.n;
    let len4 = len2 / 2;
    let len = 2 * len2;
    // Pack the Nyquist real part into the DC imaginary slot.
    spec[1] = spec[2 * len2];
    let t0 = spec[0];
    spec[0] = 0.5 * (t0 + spec[1]);
    spec[1] = 0.5 * (t0 - spec[1]);
    spec[2 * len4 + 1] = -spec[2 * len4 + 1];
    let f = 2.0 * std::f64::consts::PI / len as f64;
    for i in 1..len4 {
        let (ar, ai) = (spec[2 * i], spec[2 * i + 1]);
        let (br, bi) = (spec[2 * (len2 - i)], spec[2 * (len2 - i) + 1]);
        let t0r = 0.5 * (ar + br);
        let t0i = -0.5 * (ai - bi);
        let t1r = -0.5 * (ai + bi);
        let t1i = 0.5 * (ar - br);
        let cos = (i as f64 * f).cos() as f32;
        let sin = (((len - i * 4) as f64 / 4.0) * f).cos() as f32;
        let t2r = t1r * cos - t1i * sin;
        let t2i = t1r * sin + t1i * cos;
        spec[2 * i] = t0r + t2r;
        spec[2 * i + 1] = t2i - t0i;
        spec[2 * (len2 - i)] = t0r - t2r;
        spec[2 * (len2 - i) + 1] = t2i + t0i;
    }
    fft.inverse(&spec[..len], out);
}

/// Radix-2 complex FFT, inverse direction (`e^{+2 pi i nk / n}`), unscaled.
struct Fft {
    n: usize,
    /// `e^{+2 pi i k / n}` for `k < n / 2`.
    twiddles: Vec<(f32, f32)>,
    rev: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        debug_assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = 2.0 * std::f64::consts::PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let rev = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        Fft { n, twiddles, rev }
    }

    /// `input` and `out` are `n` complex values as interleaved re, im.
    fn inverse(&self, input: &[f32], out: &mut [f32]) {
        let n = self.n;
        for (i, &r) in self.rev.iter().enumerate() {
            out[2 * i] = input[2 * r];
            out[2 * i + 1] = input[2 * r + 1];
        }
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..half {
                    let (wr, wi) = self.twiddles[k * step];
                    let a = 2 * (start + k);
                    let b = 2 * (start + k + half);
                    let (xr, xi) = (out[b], out[b + 1]);
                    let tr = xr * wr - xi * wi;
                    let ti = xr * wi + xi * wr;
                    out[b] = out[a] - tr;
                    out[b + 1] = out[a + 1] - ti;
                    out[a] += tr;
                    out[a + 1] += ti;
                }
            }
            size *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference: x[t] = 0.5 * sum over the Hermitian-extended spectrum of
    /// X[k] e^{+2 pi i kt / N}, which is what FFmpeg's c2r with scale 0.5
    /// computes for this input layout (X[0] and X[N/2] real).
    fn naive_c2r(spec: &[f32], len: usize) -> Vec<f32> {
        (0..len)
            .map(|t| {
                let mut acc = 0f64;
                for k in 0..len {
                    let (re, im) = if k <= len / 2 {
                        (spec[2 * k] as f64, spec[2 * k + 1] as f64)
                    } else {
                        (spec[2 * (len - k)] as f64, -spec[2 * (len - k) + 1] as f64)
                    };
                    let a = 2.0 * std::f64::consts::PI * (k * t) as f64 / len as f64;
                    acc += re * a.cos() - im * a.sin();
                }
                (0.5 * acc) as f32
            })
            .collect()
    }

    #[test]
    fn inverse_rdft_matches_naive() {
        let len = 64;
        let mut seed = 7u32;
        let mut spec: Vec<f32> = (0..len + 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        spec[1] = 0.0;
        spec[len + 1] = 0.0;
        let want = naive_c2r(&spec, len);
        let mut out = vec![0f32; len];
        inverse_rdft(&Fft::new(len / 2), &mut spec, &mut out);
        for (a, b) in out.iter().zip(&want) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn frame_sizes() {
        assert_eq!(BinkAudio::new(22050, 1).block_size(), 960);
        assert_eq!(BinkAudio::new(44100, 2).block_size(), 3840);
        assert_eq!(BinkAudio::new(11025, 1).block_size(), 480);
    }

    #[test]
    fn rounding_and_clamping() {
        assert_eq!(to_i16(0.5 / 32768.0), 0);
        assert_eq!(to_i16(1.5 / 32768.0), 2);
        assert_eq!(to_i16(2.0), 32767);
        assert_eq!(to_i16(-2.0), -32768);
    }

    #[test]
    fn bad_packets_are_errors() {
        let mut b = BinkAudio::new(22050, 1);
        let mut out = Vec::new();
        assert!(b.decode_packet(&[0; 3], &mut out).is_err());
        // Size word plus fewer than 58 bits of block data.
        assert!(b.decode_packet(&[0; 8], &mut out).is_err());
        // Size word only: no blocks.
        b.decode_packet(&[0; 4], &mut out).unwrap();
        assert!(out.is_empty());
    }
}
