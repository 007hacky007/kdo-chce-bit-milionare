//! Smacker palette updates and video block decoding.

use super::bits::BitReader;
use super::huff::BigTree;
use crate::error::{FormatError, Result};

/// Expands a 6-bit palette component to 8 bits (standard Smacker table).
pub(crate) fn expand6(v: u8) -> u8 {
    let v = v & 0x3F;
    (v << 2) | (v >> 4)
}

fn pal_truncated(offset: usize) -> FormatError {
    FormatError::Truncated {
        what: "smacker palette",
        offset: offset as u64,
    }
}

/// Applies a palette chunk body (the bytes after the length byte) to `pal`.
///
/// Entries are coded against the previous palette: `1sssssss` keeps the next
/// `s + 1` entries, `01cccccc oooooooo` copies `c + 1` entries starting at
/// index `o` of the previous palette, and `00rrrrrr gg bb` sets one entry.
pub(crate) fn apply_palette(chunk: &[u8], pal: &mut [[u8; 3]; 256]) -> Result<()> {
    let old = *pal;
    let mut sz = 0usize;
    let mut p = 0usize;
    let byte = |p: &mut usize| -> Result<u8> {
        let v = *chunk.get(*p).ok_or_else(|| pal_truncated(*p))?;
        *p += 1;
        Ok(v)
    };
    while sz < 256 {
        let t = byte(&mut p)?;
        if t & 0x80 != 0 {
            sz += (t & 0x7F) as usize + 1;
        } else if t & 0x40 != 0 {
            let off = byte(&mut p)? as usize;
            let count = (t & 0x3F) as usize + 1;
            if off + count > 256 {
                return Err(FormatError::Invalid {
                    what: "smacker palette",
                    detail: format!("copy of {count} entries from {off} overruns palette"),
                });
            }
            for k in 0..count {
                if sz >= 256 {
                    break;
                }
                pal[sz] = old[off + k];
                sz += 1;
            }
        } else {
            let r = expand6(t);
            let g = expand6(byte(&mut p)?);
            let b = expand6(byte(&mut p)?);
            pal[sz] = [r, g, b];
            sz += 1;
        }
    }
    Ok(())
}

/// The four header trees, in file order.
#[derive(Clone)]
pub(crate) struct Trees {
    pub(crate) mmap: BigTree,
    pub(crate) mclr: BigTree,
    pub(crate) full: BigTree,
    pub(crate) types: BigTree,
}

impl Trees {
    /// Reads the trees from the header tree block. `sizes` are the declared
    /// table sizes for MMAP, MCLR, FULL and TYPE.
    pub(crate) fn read(data: &[u8], sizes: [u32; 4]) -> Result<Trees> {
        let mut br = BitReader::new(data);
        Ok(Trees {
            mmap: BigTree::read(&mut br, sizes[0])?,
            mclr: BigTree::read(&mut br, sizes[1])?,
            full: BigTree::read(&mut br, sizes[2])?,
            types: BigTree::read(&mut br, sizes[3])?,
        })
    }
}

const BLOCK_RUNS: [usize; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
    51, 52, 53, 54, 55, 56, 57, 58, 59, 128, 256, 512, 1024, 2048,
];

const BLK_MONO: u16 = 0;
const BLK_FULL: u16 = 1;
const BLK_VOID: u16 = 2;
// 3 = SOLID

/// The frame buffer the video stream paints into: dimensions are the coded
/// size rounded up to whole 4x4 blocks.
pub(crate) struct Canvas {
    pub(crate) pixels: Vec<u8>,
    pub(crate) stride: usize,
    pub(crate) blocks_w: usize,
    pub(crate) blocks_h: usize,
}

impl Canvas {
    pub(crate) fn new(width: u32, height: u32) -> Canvas {
        let blocks_w = (width as usize).div_ceil(4);
        let blocks_h = (height as usize).div_ceil(4);
        let stride = blocks_w * 4;
        Canvas {
            pixels: vec![0; stride * blocks_h * 4],
            stride,
            blocks_w,
            blocks_h,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pixels.fill(0);
    }
}

/// Decodes one frame's video data into `canvas`; blocks coded as VOID keep
/// the previous frame's pixels.
pub(crate) fn decode_video(
    data: &[u8],
    trees: &mut Trees,
    canvas: &mut Canvas,
    smk4: bool,
) -> Result<()> {
    trees.mmap.reset();
    trees.mclr.reset();
    trees.full.reset();
    trees.types.reset();
    let mut br = BitReader::new(data);
    let stride = canvas.stride;
    let bw = canvas.blocks_w;
    let blocks = bw * canvas.blocks_h;
    let px = &mut canvas.pixels;
    let mut blk = 0usize;
    while blk < blocks {
        let ty = trees.types.get(&mut br)?;
        let run = BLOCK_RUNS[((ty >> 2) & 0x3F) as usize].min(blocks - blk);
        match ty & 3 {
            BLK_MONO => {
                for _ in 0..run {
                    let clr = trees.mclr.get(&mut br)?;
                    let mut map = trees.mmap.get(&mut br)?;
                    let hi = (clr >> 8) as u8;
                    let lo = clr as u8;
                    let mut o = (blk / bw) * stride * 4 + (blk % bw) * 4;
                    for _ in 0..4 {
                        for x in 0..4 {
                            px[o + x] = if map & (1 << x) != 0 { hi } else { lo };
                        }
                        map >>= 4;
                        o += stride;
                    }
                    blk += 1;
                }
            }
            BLK_FULL => {
                let mode = if smk4 {
                    if br.bit()? != 0 {
                        1
                    } else if br.bit()? != 0 {
                        2
                    } else {
                        0
                    }
                } else {
                    0
                };
                for _ in 0..run {
                    let o = (blk / bw) * stride * 4 + (blk % bw) * 4;
                    full_block(&mut br, &mut trees.full, &mut px[o..], stride, mode)?;
                    blk += 1;
                }
            }
            BLK_VOID => blk += run,
            _ => {
                let col = (ty >> 8) as u8;
                for _ in 0..run {
                    let mut o = (blk / bw) * stride * 4 + (blk % bw) * 4;
                    for _ in 0..4 {
                        px[o..o + 4].fill(col);
                        o += stride;
                    }
                    blk += 1;
                }
            }
        }
    }
    Ok(())
}

/// Writes a 16-bit tree value as two pixels: low byte left, high byte right.
#[inline]
fn put2(out: &mut [u8], at: usize, v: u16) {
    out[at] = v as u8;
    out[at + 1] = (v >> 8) as u8;
}

fn full_block(
    br: &mut BitReader<'_>,
    full: &mut BigTree,
    out: &mut [u8],
    stride: usize,
    mode: u8,
) -> Result<()> {
    match mode {
        0 => {
            for row in 0..4 {
                let o = row * stride;
                let right = full.get(br)?;
                put2(out, o + 2, right);
                let left = full.get(br)?;
                put2(out, o, left);
            }
        }
        1 => {
            // Two values, each filling a 4x2 area with 2x2 pixel doubling.
            for half in 0..2 {
                let v = full.get(br)?;
                let (a, b) = (v as u8, (v >> 8) as u8);
                for row in 0..2 {
                    let o = (half * 2 + row) * stride;
                    out[o..o + 4].copy_from_slice(&[a, a, b, b]);
                }
            }
        }
        _ => {
            // Line-doubled: each pair of rows shares one decoded row.
            for half in 0..2 {
                let right = full.get(br)?;
                let left = full.get(br)?;
                for row in 0..2 {
                    let o = (half * 2 + row) * stride;
                    put2(out, o, left);
                    put2(out, o + 2, right);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_6bit_expansion() {
        assert_eq!(expand6(0x3F), 0xFF);
        assert_eq!(expand6(0x20), 0x82);
        assert_eq!(expand6(0x10), 0x41);
        assert_eq!(expand6(0), 0);
    }

    #[test]
    fn palette_skip_copy_literal() {
        let mut pal = [[0u8; 3]; 256];
        for (i, e) in pal.iter_mut().enumerate() {
            *e = [i as u8; 3];
        }
        // Literal entry 0, copy 2 entries from index 10, skip the rest.
        let chunk = [0x3F, 0x00, 0x20, 0x41, 10, 0xFF, 0xFF];
        apply_palette(&chunk, &mut pal).unwrap();
        assert_eq!(pal[0], [0xFF, 0x00, 0x82]);
        assert_eq!(pal[1], [10; 3]);
        assert_eq!(pal[2], [11; 3]);
        assert_eq!(pal[3], [3; 3]);
        assert_eq!(pal[255], [255; 3]);
    }

    #[test]
    fn palette_errors() {
        let mut pal = [[0u8; 3]; 256];
        assert!(matches!(
            apply_palette(&[0x00, 0x01], &mut pal),
            Err(FormatError::Truncated { .. })
        ));
        assert!(matches!(
            apply_palette(&[0x41, 0xFF], &mut pal),
            Err(FormatError::Invalid { .. })
        ));
    }
}
