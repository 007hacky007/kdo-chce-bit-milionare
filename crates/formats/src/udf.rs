//! Read-only UDF reader (sector size 2048, type 1 partition maps).
//!
//! Handles the Anchor VDP at sector 256, the main Volume Descriptor Sequence,
//! the File Set Descriptor, File Entries (tag 261) and Extended File Entries
//! (tag 266), short/long allocation descriptors and data embedded in the ICB.
//! Metadata partitions (UDF 2.50+), virtual and sparable maps are rejected.
//!
//! Path matching is case-insensitive using Unicode lowercase folding
//! (`char::to_lowercase`), so non-ASCII names such as accented Czech letters
//! compare equal regardless of case.
//!
//! The reader is meant for untrusted images: every length, count and depth is
//! bounded and all offset arithmetic is checked.

use crate::error::{FormatError, Result};
use std::io::{Read, Seek, SeekFrom};

const SECTOR: u64 = 2048;
const AVDP_SECTOR: u64 = 256;
const MAX_VDS_SECTORS: u64 = 256;
const MAX_PARTITION_MAPS: usize = 32;
const MAX_DIR_BYTES: u64 = 64 << 20;
const MAX_PATH_COMPONENTS: usize = 64;
const MAX_EXTENTS: usize = 1 << 20;
const MAX_AD_CHAIN: usize = 1024;
const MAX_AED_BYTES: u64 = 1 << 20;
const LEN_MASK: u32 = 0x3FFF_FFFF;

/// Marker offset used in `UdfFile::extents` for sparse (unrecorded) ranges,
/// which read back as zeros.
pub const HOLE: u64 = u64::MAX;

/// A located file. `extents` are `(absolute byte offset, length)` and cover
/// exactly `size` bytes; an offset of [`HOLE`] means zero-filled.
#[derive(Debug, Clone)]
pub struct UdfFile {
    pub size: u64,
    pub extents: Vec<(u64, u64)>,
    is_dir: bool,
}

impl UdfFile {
    /// Absolute byte offset just past the last recorded (non-[`HOLE`])
    /// extent, i.e. the image length needed to read the whole file.
    pub fn recorded_end(&self) -> u64 {
        self.extents
            .iter()
            .filter(|(abs, _)| *abs != HOLE)
            .map(|(abs, len)| abs.saturating_add(*len))
            .max()
            .unwrap_or(0)
    }
}

pub struct UdfImage<R: Read + Seek> {
    r: R,
    /// Absolute start sector of each partition map entry (by map index).
    parts: Vec<u64>,
    root: Icb,
}

#[derive(Clone, Copy)]
struct Icb {
    part: u16,
    lbn: u32,
}

fn bad(detail: impl Into<String>) -> FormatError {
    FormatError::Invalid {
        what: "udf",
        detail: detail.into(),
    }
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Validate a descriptor tag (id and checksum); returns the tag id.
fn tag_id(b: &[u8]) -> Result<u16> {
    if b.len() < 16 {
        return Err(bad("short descriptor"));
    }
    let sum = b[..16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |a, (_, v)| a.wrapping_add(*v));
    if sum != b[4] {
        return Err(bad("descriptor tag checksum mismatch"));
    }
    Ok(u16le(b, 0))
}

/// Decode an OSTA CS0 string (compression id 8 or 16).
fn decode_cs0(b: &[u8]) -> Result<String> {
    match b.first() {
        None => Ok(String::new()),
        Some(8) => Ok(b[1..].iter().map(|&c| c as char).collect()),
        Some(16) => {
            let (pairs, _) = b[1..].as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|c| u16::from_be_bytes(*c)).collect();
            Ok(String::from_utf16_lossy(&units))
        }
        Some(c) => Err(bad(format!("unknown CS0 compression id {c}"))),
    }
}

fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

impl<R: Read + Seek> UdfImage<R> {
    pub fn open(mut r: R) -> Result<Self> {
        let mut avdp = vec![0u8; SECTOR as usize];
        read_exact_at(&mut r, AVDP_SECTOR * SECTOR, &mut avdp, "udf anchor")?;
        if tag_id(&avdp)? != 2 {
            return Err(bad("no anchor volume descriptor pointer"));
        }
        let vds_len = u64::from(u32le(&avdp, 16) & LEN_MASK);
        let vds_loc = u64::from(u32le(&avdp, 20));
        let n = (vds_len / SECTOR).min(MAX_VDS_SECTORS);

        let mut part_starts: Vec<(u16, u64)> = Vec::new(); // (partition number, start sector)
        let mut lvd: Option<Vec<u8>> = None;
        let mut sec = vec![0u8; SECTOR as usize];
        for i in 0..n {
            let off = vds_loc
                .checked_add(i)
                .and_then(|s| s.checked_mul(SECTOR))
                .ok_or_else(|| bad("sector overflow"))?;
            read_exact_at(&mut r, off, &mut sec, "udf volume descriptor")?;
            let Ok(id) = tag_id(&sec) else { break };
            match id {
                5 => part_starts.push((u16le(&sec, 22), u64::from(u32le(&sec, 188)))),
                6 => lvd = Some(sec.clone()),
                8 => break,
                _ => {}
            }
        }
        let lvd = lvd.ok_or_else(|| bad("no logical volume descriptor"))?;
        if u32le(&lvd, 212) != SECTOR as u32 {
            return Err(bad("unsupported logical block size"));
        }
        let nmaps = u32le(&lvd, 268) as usize;
        let map_len = (u32le(&lvd, 264) as usize).min(SECTOR as usize - 440);
        if nmaps > MAX_PARTITION_MAPS {
            return Err(bad("too many partition maps"));
        }
        let mut parts = Vec::new();
        let mut o = 440usize;
        for _ in 0..nmaps {
            if o + 2 > 440 + map_len {
                return Err(bad("truncated partition map table"));
            }
            let (ty, len) = (lvd[o], lvd[o + 1] as usize);
            if len < 2 || o + len > 440 + map_len {
                return Err(bad("bad partition map"));
            }
            match ty {
                1 if len >= 6 => {
                    let pn = u16le(&lvd, o + 4);
                    let start = part_starts
                        .iter()
                        .find(|(n, _)| *n == pn)
                        .ok_or_else(|| bad("partition map refers to missing partition"))?
                        .1;
                    parts.push(start);
                }
                2 => {
                    let id = lvd.get(o + 5..o + 36).unwrap_or(&[]);
                    let id = String::from_utf8_lossy(id);
                    return Err(if id.contains("Metadata") {
                        bad("metadata partition unsupported")
                    } else {
                        bad("virtual/sparable partition map unsupported")
                    });
                }
                _ => return Err(bad("unsupported partition map type")),
            }
            o += len;
        }
        if parts.is_empty() {
            return Err(bad("no partition maps"));
        }

        // File Set Descriptor location: long_ad at LVD offset 248.
        let fsd_lbn = u32le(&lvd, 252);
        let fsd_part = u16le(&lvd, 256);
        let mut img = UdfImage {
            r,
            parts,
            root: Icb { part: 0, lbn: 0 },
        };
        let fsd_off = img.block_offset(fsd_part, fsd_lbn)?;
        let mut fsd = vec![0u8; SECTOR as usize];
        img.read_exact(fsd_off, &mut fsd, "udf file set descriptor")?;
        if tag_id(&fsd)? != 256 {
            return Err(bad("expected file set descriptor"));
        }
        img.root = Icb {
            part: u16le(&fsd, 408),
            lbn: u32le(&fsd, 404),
        };
        Ok(img)
    }

    fn read_exact(&mut self, off: u64, buf: &mut [u8], what: &'static str) -> Result<()> {
        read_exact_at(&mut self.r, off, buf, what)
    }

    fn block_offset(&self, part: u16, lbn: u32) -> Result<u64> {
        let start = *self
            .parts
            .get(part as usize)
            .ok_or_else(|| bad("partition reference out of range"))?;
        start
            .checked_add(u64::from(lbn))
            .and_then(|s| s.checked_mul(SECTOR))
            .ok_or_else(|| bad("sector overflow"))
    }

    /// Parse the File Entry / Extended File Entry at `icb`.
    fn load_entry(&mut self, icb: Icb) -> Result<UdfFile> {
        let base = self.block_offset(icb.part, icb.lbn)?;
        let mut fe = vec![0u8; SECTOR as usize];
        self.read_exact(base, &mut fe, "udf file entry")?;
        let (ea_off, ad_len_off, ad_base) = match tag_id(&fe)? {
            261 => (168usize, 172usize, 176usize),
            266 => (208, 212, 216),
            _ => return Err(bad("expected file entry")),
        };
        let file_type = fe[27];
        let flags = u16le(&fe, 34);
        let size = u64le(&fe, 56);
        let l_ea = u32le(&fe, ea_off) as usize;
        let l_ad = u32le(&fe, ad_len_off) as usize;
        let ad_start = ad_base
            .checked_add(l_ea)
            .filter(|s| *s <= fe.len())
            .ok_or_else(|| bad("extended attributes overflow entry"))?;
        let ad_end = ad_start
            .checked_add(l_ad)
            .filter(|e| *e <= fe.len())
            .ok_or_else(|| bad("allocation descriptors overflow entry"))?;
        let ads = &fe[ad_start..ad_end];

        let mut extents: Vec<(u64, u64)> = Vec::new();
        match flags & 7 {
            3 => {
                let len = size.min(l_ad as u64);
                if len != size {
                    return Err(bad("embedded data shorter than file size"));
                }
                extents.push((base + ad_start as u64, len));
            }
            ty @ (0 | 1) => {
                let mut covered = 0u64;
                let mut chain = 0usize;
                let mut cur: Vec<u8> = ads.to_vec();
                loop {
                    let step = if ty == 0 { 8 } else { 16 };
                    let mut next: Option<(u16, u32, u64)> = None;
                    for ad in cur.chunks_exact(step) {
                        let raw = u32le(ad, 0);
                        let len = u64::from(raw & LEN_MASK);
                        let kind = raw >> 30;
                        let (lbn, part) =
                            (u32le(ad, 4), if ty == 1 { u16le(ad, 8) } else { icb.part });
                        if len == 0 {
                            break;
                        }
                        if kind == 3 {
                            next = Some((part, lbn, len));
                            break;
                        }
                        if covered >= size {
                            break;
                        }
                        let take = len.min(size - covered);
                        let off = if kind == 0 {
                            self.block_offset(part, lbn)?
                        } else {
                            HOLE
                        };
                        if extents.len() >= MAX_EXTENTS {
                            return Err(bad("too many extents"));
                        }
                        extents.push((off, take));
                        covered += take;
                    }
                    let Some((part, lbn, len)) = next else { break };
                    chain += 1;
                    if chain > MAX_AD_CHAIN || len > MAX_AED_BYTES {
                        return Err(bad("allocation extent chain too long"));
                    }
                    let off = self.block_offset(part, lbn)?;
                    let mut aed = vec![0u8; len as usize];
                    self.read_exact(off, &mut aed, "udf allocation extent")?;
                    if aed.len() < 24 || tag_id(&aed)? != 258 {
                        return Err(bad("expected allocation extent descriptor"));
                    }
                    let n = (u32le(&aed, 20) as usize).min(aed.len() - 24);
                    cur = aed[24..24 + n].to_vec();
                }
                if covered != size {
                    return Err(bad("allocation descriptors do not cover file size"));
                }
            }
            _ => return Err(bad("unsupported allocation descriptor type")),
        }
        Ok(UdfFile {
            size,
            extents,
            is_dir: file_type == 4,
        })
    }

    fn read_all(&mut self, f: &UdfFile) -> Result<Vec<u8>> {
        if f.size > MAX_DIR_BYTES {
            return Err(bad("directory too large"));
        }
        let mut v = vec![0u8; f.size as usize];
        self.read_at(f, 0, &mut v)?;
        Ok(v)
    }

    /// Find the child `name` in directory `dir` (case-insensitive).
    fn lookup(&mut self, dir: &UdfFile, name: &str) -> Result<Option<Icb>> {
        let data = self.read_all(dir)?;
        lookup_in(&data, name)
    }

    /// Look up `path` (components separated by `/`, case-insensitive).
    pub fn find(&mut self, path: &str) -> Result<Option<UdfFile>> {
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        if comps.is_empty() {
            return Ok(None);
        }
        if comps.len() > MAX_PATH_COMPONENTS {
            return Err(bad("path too deep"));
        }
        let mut cur = self.load_entry(self.root)?;
        for (i, c) in comps.iter().enumerate() {
            if !cur.is_dir {
                return Ok(None);
            }
            let Some(icb) = self.lookup(&cur, c)? else {
                return Ok(None);
            };
            cur = self.load_entry(icb)?;
            if i + 1 == comps.len() {
                return Ok(Some(cur));
            }
        }
        Ok(None)
    }

    /// Fill `buf` with the file bytes starting at `offset`.
    pub fn read_at(&mut self, f: &UdfFile, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|e| *e <= f.size)
            .ok_or(FormatError::Truncated {
                what: "udf file read",
                offset,
            })?;
        let mut ext_start = 0u64;
        for &(abs, len) in &f.extents {
            let ext_end = ext_start.saturating_add(len);
            if ext_end > offset && ext_start < end {
                let from = offset.max(ext_start);
                let to = end.min(ext_end);
                let dst = &mut buf[(from - offset) as usize..(to - offset) as usize];
                if abs == HOLE {
                    dst.fill(0);
                } else {
                    let pos = abs
                        .checked_add(from - ext_start)
                        .ok_or_else(|| bad("offset overflow"))?;
                    read_exact_at(&mut self.r, pos, dst, "udf file data")?;
                }
            }
            ext_start = ext_end;
            if ext_start >= end {
                break;
            }
        }
        Ok(())
    }
}

fn read_exact_at<R: Read + Seek>(
    r: &mut R,
    off: u64,
    buf: &mut [u8],
    what: &'static str,
) -> Result<()> {
    let io = |source| FormatError::Io {
        what,
        offset: off,
        source,
    };
    r.seek(SeekFrom::Start(off)).map_err(io)?;
    r.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FormatError::Truncated { what, offset: off }
        } else {
            io(e)
        }
    })
}

/// Scan raw directory data for `name`. A zero tag ends the data (padded
/// directories); entries whose names cannot be decoded are skipped.
fn lookup_in(data: &[u8], name: &str) -> Result<Option<Icb>> {
    let want = fold(name);
    let mut p = 0usize;
    while p + 38 <= data.len() {
        if u16le(data, p) == 0 {
            break;
        }
        if tag_id(&data[p..p + 16])? != 257 {
            return Err(bad("expected file identifier descriptor"));
        }
        let chars = data[p + 18];
        let l_fi = data[p + 19] as usize;
        let l_iu = u16le(data, p + 36) as usize;
        let name_at = p + 38 + l_iu;
        let end = name_at + l_fi;
        if end > data.len() {
            return Err(bad("file identifier overflows directory"));
        }
        if chars & 0x0C == 0
            && let Ok(child) = decode_cs0(&data[name_at..end])
            && fold(&child) == want
        {
            return Ok(Some(Icb {
                lbn: u32le(data, p + 24),
                part: u16le(data, p + 28),
            }));
        }
        p = (end + 3) & !3;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid(name: &[u8], lbn: u32) -> Vec<u8> {
        let mut v = vec![0u8; 38];
        v[0] = 1;
        v[1] = 1; // tag id 257
        v[19] = name.len() as u8;
        v[24..28].copy_from_slice(&lbn.to_le_bytes());
        v.extend_from_slice(name);
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
        let sum = v[..16]
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4)
            .fold(0u8, |a, (_, b)| a.wrapping_add(*b));
        v[4] = sum;
        v
    }

    #[test]
    fn lookup_stops_at_zero_padding() {
        let mut d = fid(b"\x08Abc", 7);
        d.extend(vec![0u8; 512]);
        assert_eq!(lookup_in(&d, "abc").unwrap().unwrap().lbn, 7);
        assert!(lookup_in(&d, "zzz").unwrap().is_none());
    }

    #[test]
    fn lookup_skips_undecodable_name() {
        let mut d = fid(b"\x07junk", 1);
        d.extend(fid(b"\x08Good", 9));
        assert_eq!(lookup_in(&d, "GOOD").unwrap().unwrap().lbn, 9);
    }
}
