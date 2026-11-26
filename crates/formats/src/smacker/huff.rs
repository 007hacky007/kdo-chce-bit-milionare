//! Smacker Huffman trees: 8-bit byte trees and the 16-bit "big" trees with
//! three escape codes acting as a small most-recently-used cache.
//!
//! Both kinds are stored as a flat pre-order array. An inner node holds
//! `NODE | n` where `n` is the number of entries in its left subtree, so the
//! left child sits at `i + 1` and the right child at `i + 1 + n`. A leaf holds
//! its value. A 0 bit selects the left branch, a 1 bit the right one.

use super::bits::BitReader;
use crate::error::{FormatError, Result};

const NODE: u32 = 0x8000_0000;
/// Maximum code length accepted for any tree (matches the reference decoder).
const MAX_DEPTH: u32 = 32;

fn invalid(detail: &str) -> FormatError {
    FormatError::Invalid {
        what: "smacker tree",
        detail: detail.to_string(),
    }
}

/// Walks a flat tree from the root to a leaf and returns the leaf index.
#[inline]
fn walk(values: &[u32], br: &mut BitReader<'_>) -> Result<usize> {
    let mut i = 0usize;
    loop {
        let e = values[i];
        if e & NODE == 0 {
            return Ok(i);
        }
        if br.bit()? != 0 {
            i += (e & !NODE) as usize;
        }
        i += 1;
    }
}

/// A tree over 8-bit symbols, used to build the leaves of a big tree and
/// by the Smacker audio decoder. A tree with a single leaf consumes no bits.
pub(crate) struct ByteTree {
    values: Vec<u32>,
}

impl ByteTree {
    pub(crate) fn read(br: &mut BitReader<'_>) -> Result<ByteTree> {
        let mut values = Vec::new();
        let mut leaves = 0usize;
        Self::read_node(br, &mut values, &mut leaves, 0)?;
        Ok(ByteTree { values })
    }

    fn read_node(
        br: &mut BitReader<'_>,
        out: &mut Vec<u32>,
        leaves: &mut usize,
        depth: u32,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("byte tree too deep"));
        }
        if br.bit()? == 0 {
            if *leaves >= 256 {
                return Err(invalid("byte tree has more than 256 leaves"));
            }
            *leaves += 1;
            out.push(br.bits(8)?);
        } else {
            let t = out.len();
            out.push(NODE);
            Self::read_node(br, out, leaves, depth + 1)?;
            out[t] = NODE | (out.len() - t - 1) as u32;
            Self::read_node(br, out, leaves, depth + 1)?;
        }
        Ok(())
    }

    pub(crate) fn get(&self, br: &mut BitReader<'_>) -> Result<u32> {
        Ok(self.values[walk(&self.values, br)?])
    }
}

/// A tree over 16-bit values with a three-entry recency cache.
#[derive(Clone)]
pub(crate) struct BigTree {
    values: Vec<u32>,
    last: [usize; 3],
}

struct BuildCtx<'t> {
    lo: Option<&'t ByteTree>,
    hi: Option<&'t ByteTree>,
    escapes: [u32; 3],
    last: [Option<usize>; 3],
    limit: usize,
}

impl BigTree {
    /// Reads one header tree. `alloc_size` is the byte size the header
    /// declares for the decoded table; it bounds the number of entries.
    pub(crate) fn read(br: &mut BitReader<'_>, alloc_size: u32) -> Result<BigTree> {
        if br.bit()? == 0 {
            // Tree absent: every lookup yields 0 and consumes no bits.
            return Ok(BigTree {
                values: vec![0, 0],
                last: [1, 1, 1],
            });
        }
        let mut subtrees = [None, None];
        for t in &mut subtrees {
            if br.bit()? != 0 {
                *t = Some(ByteTree::read(br)?);
                br.bit()?; // terminating 0 bit
            }
        }
        let mut escapes = [0u32; 3];
        for e in &mut escapes {
            *e = br.bits(16)?;
        }
        // Same bound as FFmpeg, `((size + 3) >> 2) + 4` entries. Each entry
        // also costs at least one bit, so the bitstream bounds the size as
        // well; the header value keeps hostile files from building huge tables.
        let limit = ((alloc_size as usize).div_ceil(4) + 4).min(br.bits_left() + 4);
        let mut ctx = BuildCtx {
            lo: subtrees[0].as_ref(),
            hi: subtrees[1].as_ref(),
            escapes,
            last: [None; 3],
            limit,
        };
        let mut values = Vec::new();
        Self::read_node(br, &mut values, &mut ctx, 0)?;
        br.bit()?; // terminating 0 bit
        let mut last = [0usize; 3];
        for (slot, found) in last.iter_mut().zip(ctx.last) {
            *slot = match found {
                Some(i) => i,
                None => {
                    values.push(0);
                    values.len() - 1
                }
            };
        }
        Ok(BigTree { values, last })
    }

    fn read_node(
        br: &mut BitReader<'_>,
        out: &mut Vec<u32>,
        ctx: &mut BuildCtx<'_>,
        depth: u32,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("big tree too deep"));
        }
        if out.len() + 1 >= ctx.limit {
            return Err(invalid("big tree larger than declared size"));
        }
        if br.bit()? == 0 {
            let lo = match ctx.lo {
                Some(t) => t.get(br)?,
                None => 0,
            };
            let hi = match ctx.hi {
                Some(t) => t.get(br)?,
                None => 0,
            };
            let mut val = lo | (hi << 8);
            if let Some(k) = ctx.escapes.iter().position(|&e| e == val) {
                ctx.last[k] = Some(out.len());
                val = 0;
            }
            out.push(val);
        } else {
            let t = out.len();
            out.push(NODE);
            Self::read_node(br, out, ctx, depth + 1)?;
            out[t] = NODE | (out.len() - t - 1) as u32;
            Self::read_node(br, out, ctx, depth + 1)?;
        }
        Ok(())
    }

    /// Clears the three cache entries; done at the start of every frame.
    pub(crate) fn reset(&mut self) {
        for &l in &self.last {
            self.values[l] = 0;
        }
    }

    /// Decodes one value and updates the recency cache.
    #[inline]
    pub(crate) fn get(&mut self, br: &mut BitReader<'_>) -> Result<u16> {
        let v = self.values[walk(&self.values, br)?];
        let [l0, l1, l2] = self.last;
        if v != self.values[l0] {
            self.values[l2] = self.values[l1];
            self.values[l1] = self.values[l0];
            self.values[l0] = v;
        }
        Ok(v as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs bits (first element first) into LSB-first bytes.
    fn pack(bits: &[u32]) -> Vec<u8> {
        let mut out = vec![0u8; bits.len().div_ceil(8) + 4];
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

    #[test]
    fn absent_tree_yields_zero_without_reading() {
        let data = pack(&[0]);
        let mut br = BitReader::new(&data);
        let mut t = BigTree::read(&mut br, 0).unwrap();
        let mut empty = BitReader::new(&[]);
        assert_eq!(t.get(&mut empty).unwrap(), 0);
    }

    #[test]
    fn big_tree_with_escape_cache() {
        // Both trees use the shape node(node(a, b), node(c, node(d, e))), so
        // the codes are a=00, b=01, c=10, d=110, e=111.
        fn tree(b: &mut Vec<u32>, leaf: &mut dyn FnMut(&mut Vec<u32>, usize)) {
            b.extend([1, 1, 0]);
            leaf(b, 0);
            b.push(0);
            leaf(b, 1);
            b.extend([1, 0]);
            leaf(b, 2);
            b.extend([1, 0]);
            leaf(b, 3);
            b.push(0);
            leaf(b, 4);
        }
        let lo = [0x11u32, 0x22, 0x33, 0x44, 0x55];
        let code: [&[u32]; 5] = [&[0, 0], &[0, 1], &[1, 0], &[1, 1, 0], &[1, 1, 1]];
        let mut b = vec![1, 1];
        tree(&mut b, &mut |b, i| push_bits(b, lo[i], 8));
        b.push(0); // byte tree end
        b.push(0); // no high tree
        // Escapes: 0x33 -> slot 0, 0x44 -> slot 1, 0x55 -> slot 2.
        for e in [0x33u32, 0x44, 0x55] {
            push_bits(&mut b, e, 16);
        }
        // Big tree, same shape: leaf i holds the low byte coded as lo[i], so
        // leaves 2, 3 and 4 become cache slots 0, 1 and 2.
        tree(&mut b, &mut |b, i| b.extend_from_slice(code[i]));
        b.push(0);
        let data = pack(&b);
        let mut br = BitReader::new(&data);
        let mut t = BigTree::read(&mut br, 64).unwrap();
        t.reset();

        // (leaf, expected value); the cache after each step is in comments.
        let steps = [
            (0, 0x11), // [11, 0, 0]
            (1, 0x22), // [22, 11, 0]
            (4, 0x00), // slot 2 -> [0, 22, 11]
            (4, 0x11), // slot 2 -> [11, 0, 22]
            (3, 0x00), // slot 1 -> [0, 11, 0]
            (2, 0x00), // slot 0, equal to slot 0: unchanged
            (3, 0x11), // slot 1 -> [11, 0, 11]
            (2, 0x11), // slot 0
        ];
        let bits: Vec<u32> = steps.iter().flat_map(|&(l, _)| code[l].to_vec()).collect();
        let stream = pack(&bits);
        let mut sr = BitReader::new(&stream);
        for (n, &(leaf, want)) in steps.iter().enumerate() {
            assert_eq!(t.get(&mut sr).unwrap(), want, "step {n} (leaf {leaf})");
        }
        // A frame reset clears the cache again.
        t.reset();
        let stream = pack(code[4]);
        assert_eq!(t.get(&mut BitReader::new(&stream)).unwrap(), 0);
    }

    #[test]
    fn deep_tree_rejected() {
        let data = vec![0xFF; 64];
        let mut br = BitReader::new(&data);
        assert!(BigTree::read(&mut br, 1 << 20).is_err());
    }
}
