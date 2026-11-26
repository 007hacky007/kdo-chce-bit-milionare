//! The list of files a release installs and where they live in `Setup.exe`.

/// One installed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Relative path, `/` separated, without the `app/` prefix.
    pub path: &'static str,
    /// Absolute offset of the stored (uncompressed) payload in `Setup.exe`.
    pub setup_offset: u64,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// A release: its `Setup.exe` identity and the files it installs.
///
/// `files` must be sorted by `setup_offset`; ranges must not overlap except
/// for identical files sharing one payload (same offset and size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    pub setup_size: u64,
    pub setup_sha256: [u8; 32],
    pub files: &'static [ManifestEntry],
}

impl Manifest {
    /// Sum of all file sizes.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TOPCD;

    #[test]
    fn topcd_has_88_sorted_files() {
        assert_eq!(TOPCD.files.len(), 88);
        assert!(
            TOPCD
                .files
                .windows(2)
                .all(|w| w[0].setup_offset <= w[1].setup_offset)
        );
        assert!(TOPCD.files.iter().all(|f| !f.path.contains('\\')));
        assert!(TOPCD.files.iter().any(|f| f.path == "Milionar.exe"));
    }

    #[test]
    fn hex_formats_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0x7f]), "00ab7f");
    }
}
