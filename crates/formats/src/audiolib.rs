//! Audiolib archive index (`audio1.dat`, `audio2.dat`).
//!
//! Layout: `u32 count`, 1024 name slots of 32 bytes (`stem NUL ext NUL...`),
//! 1024 absolute `u32` offsets, 1024 `u32` sizes, then contiguous RIFF WAVE
//! payloads starting at `0xA004`.

use crate::{FormatError, Result};

pub const HEADER_LEN: usize = 0xA004;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioEntry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioLibIndex {
    pub entries: Vec<AudioEntry>,
}

const MAX_ENTRIES: usize = 1024;
const NAMES_AT: usize = 0x4;
const NAME_LEN: usize = 32;
const OFFSETS_AT: usize = 0x8004;
const SIZES_AT: usize = 0x9004;

fn invalid(detail: String) -> FormatError {
    FormatError::Invalid {
        what: "audiolib",
        detail,
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn parse_name(index: usize, slot: &[u8]) -> Result<String> {
    let nul = slot
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| invalid(format!("entry {index}: name has no NUL")))?;
    let ext = &slot[nul + 1..];
    let ext = &ext[..ext.iter().position(|&b| b == 0).unwrap_or(ext.len())];
    let latin1 = |b: &[u8]| b.iter().map(|&c| c as char).collect::<String>();
    Ok(format!("{}.{}", latin1(&slot[..nul]), latin1(ext)))
}

impl AudioLibIndex {
    /// Parses the index from the first [`HEADER_LEN`] bytes of an archive.
    pub fn parse(header: &[u8]) -> Result<AudioLibIndex> {
        if header.len() < HEADER_LEN {
            return Err(FormatError::Truncated {
                what: "audiolib header",
                offset: header.len() as u64,
            });
        }
        let count = u32_at(header, 0) as usize;
        if count > MAX_ENTRIES {
            return Err(invalid(format!(
                "entry count {count} exceeds {MAX_ENTRIES}"
            )));
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let at = NAMES_AT + i * NAME_LEN;
            let name = parse_name(i, &header[at..at + NAME_LEN])?;
            let offset = u32_at(header, OFFSETS_AT + i * 4);
            let size = u32_at(header, SIZES_AT + i * 4);
            if (offset as usize) < HEADER_LEN {
                return Err(invalid(format!(
                    "entry {i}: offset {offset:#x} inside header"
                )));
            }
            entries.push(AudioEntry { name, offset, size });
        }
        Ok(AudioLibIndex { entries })
    }

    /// ASCII case-insensitive lookup by full name, e.g. `"Char0100.wav"`.
    pub fn find(&self, name: &str) -> Option<&AudioEntry> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
}

/// Slices an entry's payload out of the whole archive, checking bounds.
pub fn entry_bytes<'a>(file: &'a [u8], e: &AudioEntry) -> Result<&'a [u8]> {
    let end = u64::from(e.offset) + u64::from(e.size);
    usize::try_from(end)
        .ok()
        .and_then(|end| file.get(e.offset as usize..end))
        .ok_or(FormatError::Truncated {
            what: "audiolib entry",
            offset: end,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(entries: &[(&[u8], u32, u32)]) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        for (i, (name, off, size)) in entries.iter().enumerate() {
            h[4 + i * 32..4 + i * 32 + name.len()].copy_from_slice(name);
            h[0x8004 + i * 4..0x8004 + i * 4 + 4].copy_from_slice(&off.to_le_bytes());
            h[0x9004 + i * 4..0x9004 + i * 4 + 4].copy_from_slice(&size.to_le_bytes());
        }
        h
    }

    #[test]
    fn audiolib_parses_two_entries() {
        let h = header(&[
            (b"Char0100\0wav", 0xA004, 100),
            (b"Char1100q\0wav", 0xA068, 50),
        ]);
        let idx = AudioLibIndex::parse(&h).unwrap();
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.entries[0].name, "Char0100.wav");
        assert_eq!(idx.entries[0].offset, 0xA004);
        assert_eq!(idx.entries[0].size, 100);
        assert_eq!(idx.entries[1].name, "Char1100q.wav");
        assert_eq!(idx.entries[1].offset, 0xA068);
        assert_eq!(idx.entries[1].size, 50);
        assert!(idx.find("char0100.WAV").is_some());
        assert!(idx.find("Char9999.wav").is_none());
    }

    #[test]
    fn audiolib_rejects_count_over_1024() {
        let mut h = header(&[]);
        h[0..4].copy_from_slice(&1025u32.to_le_bytes());
        assert!(matches!(
            AudioLibIndex::parse(&h),
            Err(FormatError::Invalid { .. })
        ));
    }

    #[test]
    fn audiolib_truncated_header_errors() {
        assert!(matches!(
            AudioLibIndex::parse(&[0u8; 100]),
            Err(FormatError::Truncated { .. })
        ));
    }

    #[test]
    fn audiolib_rejects_low_offset_and_missing_nul() {
        let h = header(&[(b"Char0100\0wav", 0x100, 10)]);
        assert!(matches!(
            AudioLibIndex::parse(&h),
            Err(FormatError::Invalid { .. })
        ));
        let mut h = header(&[(b"x", 0xA004, 10)]);
        h[4..36].fill(b'a');
        assert!(matches!(
            AudioLibIndex::parse(&h),
            Err(FormatError::Invalid { .. })
        ));
    }

    #[test]
    fn entry_bytes_checks_bounds() {
        let e = AudioEntry {
            name: "a.wav".into(),
            offset: 4,
            size: 4,
        };
        assert_eq!(
            entry_bytes(&[0, 1, 2, 3, 4, 5, 6, 7], &e).unwrap(),
            &[4, 5, 6, 7]
        );
        assert!(entry_bytes(&[0; 7], &e).is_err());
        let big = AudioEntry {
            name: "b.wav".into(),
            offset: u32::MAX,
            size: u32::MAX,
        };
        assert!(entry_bytes(&[0; 8], &big).is_err());
    }
}
