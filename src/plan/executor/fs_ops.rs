use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::scanner::chd;
use crate::util::verify_sha1;

/// Verify a written file against its catalogued SHA1.
///
/// A CHD is catalogued by its *internal* (logical-data) SHA1, read from the
/// header — not by the hash of the `.chd` file's bytes, which changes with the
/// compression used. Hashing the whole file would never match the catalogue, so
/// a `.chd` is verified by re-reading its header SHA1; every other file is a
/// full-file hash. A byte-for-byte copy or rename preserves the header, so the
/// internal SHA1 is exactly as strong a check here as a content hash is for a
/// loose ROM.
pub(super) fn verify_written_sha1(path: &Path, expected: &str) -> Result<bool> {
    if chd::is_chd_path(path) {
        Ok(chd::read_chd_sha1(path)?.eq_ignore_ascii_case(expected))
    } else {
        verify_sha1(path, expected)
    }
}

pub(super) fn validate_output_path(path: &Path) -> Result<()> {
    for component in path.components() {
        match component {
            std::path::Component::ParentDir | std::path::Component::CurDir => {
                anyhow::bail!(
                    "output path contains an unsafe component: {}",
                    path.display()
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// SHA-1 of a whole file, upper-case hex.
pub(super) fn hash_file(path: &Path) -> Result<String> {
    use sha1::{Digest, Sha1};
    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut hasher = Sha1::new();
    Digest::update(&mut hasher, &bytes);
    Ok(crate::util::hex_upper(Digest::finalize(hasher)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn verify_written_sha1_uses_internal_hash_for_chd() {
        let temp = TempDir::new().unwrap();
        let chd = temp.path().join("disk.chd");
        // A minimal valid v5 CHD header carrying a chosen internal SHA1 at offset
        // 84. The .chd file's own bytes hash to something else entirely.
        let mut header = vec![0u8; 124];
        header[0..8].copy_from_slice(b"MComprHD");
        header[8..12].copy_from_slice(&124u32.to_be_bytes());
        header[12..16].copy_from_slice(&5u32.to_be_bytes());
        header[84..104].copy_from_slice(&[0x11u8; 20]);
        fs::write(&chd, &header).unwrap();

        let internal = "1111111111111111111111111111111111111111";
        // Verified against the internal (header) SHA1, case-insensitively — the
        // bug was hashing the whole file, which never matches.
        assert!(verify_written_sha1(&chd, internal).unwrap());
        assert!(verify_written_sha1(&chd, &internal.to_uppercase()).unwrap());
        assert!(!verify_written_sha1(&chd, "0000000000000000000000000000000000000000").unwrap());

        // A non-CHD file still verifies by its full-file content hash.
        let rom = temp.path().join("a.rom");
        fs::write(&rom, b"abc").unwrap();
        assert!(verify_written_sha1(&rom, "a9993e364706816aba3e25717850c26c9cd0d89d").unwrap());
        assert!(!verify_written_sha1(&rom, internal).unwrap());
    }
}
