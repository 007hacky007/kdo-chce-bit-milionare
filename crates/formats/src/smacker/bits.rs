//! LSB-first bit reader used by the Smacker trees and video stream.

use crate::error::{FormatError, Result};

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Reads one bit; bits are taken from the least significant end of each byte.
    #[inline]
    pub(crate) fn bit(&mut self) -> Result<u32> {
        match self.data.get(self.pos >> 3) {
            Some(&b) => {
                let v = (b >> (self.pos & 7)) & 1;
                self.pos += 1;
                Ok(v as u32)
            }
            None => Err(self.truncated()),
        }
    }

    /// Reads `n` (at most 32) bits; the first bit read is the least significant.
    pub(crate) fn bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        if self.bits_left() < n as usize {
            return Err(self.truncated());
        }
        if n == 0 {
            return Ok(0);
        }
        // Up to 39 bits starting at the current byte cover any n <= 32.
        let at = self.pos >> 3;
        let end = (at + 5).min(self.data.len());
        let mut word = 0u64;
        for (i, &b) in self.data[at..end].iter().enumerate() {
            word |= u64::from(b) << (8 * i);
        }
        let v = (word >> (self.pos & 7)) & ((1u64 << n) - 1);
        self.pos += n as usize;
        Ok(v as u32)
    }

    /// Number of bits consumed so far.
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Skips `n` bits; fails without moving when fewer are left.
    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        if self.bits_left() < n {
            return Err(self.truncated());
        }
        self.pos += n;
        Ok(())
    }

    pub(crate) fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    fn truncated(&self) -> FormatError {
        FormatError::Truncated {
            what: "smacker bitstream",
            offset: (self.pos / 8) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_lsb_first() {
        let mut br = BitReader::new(&[0b1010_0110, 0x34, 0x12]);
        let first: Vec<u32> = (0..4).map(|_| br.bit().unwrap()).collect();
        assert_eq!(first, [0, 1, 1, 0]);
        assert_eq!(br.bits(4).unwrap(), 0b1010);
        assert_eq!(br.bits(16).unwrap(), 0x1234);
        assert!(matches!(br.bit(), Err(FormatError::Truncated { .. })));
    }
}
