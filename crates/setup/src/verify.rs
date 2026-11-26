//! Re-hashes an imported `game/` tree against a manifest.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::manifest::hex;
use crate::source::find_path;
use crate::{ImportError, Manifest};

/// Checks that every manifest file exists under `data_dir/game` with the
/// expected size and SHA-256. Names are looked up ASCII case-insensitively.
pub fn verify(data_dir: &Path, manifest: &Manifest) -> Result<(), ImportError> {
    let game = data_dir.join("game");
    let mut buf = vec![0u8; 1 << 20];
    for entry in manifest.files {
        let path = find_path(&game, entry.path)
            .filter(|p| p.is_file())
            .ok_or_else(|| ImportError::Missing(entry.path.into()))?;
        let mut file = File::open(&path).map_err(ImportError::io(&path))?;
        let len = file.metadata().map_err(ImportError::io(&path))?.len();
        if len != entry.size {
            return Err(ImportError::Mismatch {
                file: entry.path.into(),
                expected: format!("{} bytes", entry.size),
                actual: format!("{len} bytes"),
            });
        }
        let mut h = Sha256::new();
        loop {
            let n = file.read(&mut buf).map_err(ImportError::io(&path))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        let actual: [u8; 32] = h.finalize().into();
        if actual != entry.sha256 {
            return Err(ImportError::Mismatch {
                file: entry.path.into(),
                expected: hex(&entry.sha256),
                actual: hex(&actual),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManifestEntry;
    use std::fs;

    fn manifest(files: Vec<ManifestEntry>) -> Manifest {
        Manifest {
            setup_size: 0,
            setup_sha256: [0; 32],
            files: Box::leak(files.into_boxed_slice()),
        }
    }

    fn entry(path: &'static str, data: &[u8]) -> ManifestEntry {
        ManifestEntry {
            path,
            setup_offset: 0,
            size: data.len() as u64,
            sha256: Sha256::digest(data).into(),
        }
    }

    #[test]
    fn verify_ok_missing_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game/Db");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("a.dat"), b"hello").unwrap();
        let m = manifest(vec![entry("db/A.dat", b"hello")]);
        verify(dir.path(), &m).unwrap();

        let m = manifest(vec![entry("Db/b.dat", b"x")]);
        assert!(matches!(
            verify(dir.path(), &m),
            Err(ImportError::Missing(_))
        ));

        let m = manifest(vec![entry("Db/a.dat", b"jello")]);
        assert!(matches!(
            verify(dir.path(), &m),
            Err(ImportError::Mismatch { .. })
        ));
        let m = manifest(vec![entry("Db/a.dat", b"hello!")]);
        assert!(matches!(
            verify(dir.path(), &m),
            Err(ImportError::Mismatch { .. })
        ));
    }
}
