//! CHD (Compressed Hunks of Data) header reading.
//!
//! A MAME/Redump DAT references a CHD by its *internal* SHA1 — the hash of the
//! disk's logical (uncompressed) data, stored in the CHD header — not the SHA1
//! of the `.chd` file's bytes. The file hash changes with the compression used;
//! the internal hash does not. To match a `.chd` on disk against a `<disk>` DAT
//! entry we must read that internal SHA1 out of the header rather than hashing
//! the file.
//!
//! Only CHD **v5** is supported — the only version present in the current sets
//! (verified across all 209 CHDs in the MAME + Demul collections). An older or
//! newer version, a bad magic, or a truncated header is an error so the CHD
//! surfaces in the scan summary rather than being silently mis-identified.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Every CHD file begins with this 8-byte tag.
const CHD_MAGIC: &[u8; 8] = b"MComprHD";

/// The CHD v5 header is 124 bytes.
const V5_HEADER_LEN: usize = 124;

/// Byte offset of the overall logical-data SHA1 within a v5 header (20 bytes).
/// Layout: tag[8] len[4] version[4] compressors[16] logicalbytes[8] mapoffset[8]
/// metaoffset[8] hunkbytes[4] unitbytes[4] rawsha1[20] sha1[20] parentsha1[20].
const V5_SHA1_OFFSET: usize = 84;

/// Whether a path looks like a CHD by extension (case-insensitive).
pub fn is_chd_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("chd"))
}

/// Read a CHD's internal (logical-data) SHA1 as uppercase hex.
pub fn read_chd_sha1(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open CHD {}", path.display()))?;
    let mut header = [0u8; V5_HEADER_LEN];
    file.read_exact(&mut header)
        .with_context(|| format!("read CHD header {}", path.display()))?;

    parse_v5_sha1(&header).with_context(|| format!("parse CHD header {}", path.display()))
}

/// Extract the v5 SHA1 from a 124-byte header buffer.
fn parse_v5_sha1(header: &[u8; V5_HEADER_LEN]) -> Result<String> {
    if &header[0..8] != CHD_MAGIC {
        bail!("not a CHD (bad magic)");
    }
    let version = u32::from_be_bytes([header[12], header[13], header[14], header[15]]);
    if version != 5 {
        bail!("unsupported CHD version {version} (only v5 supported)");
    }
    Ok(crate::util::hex_upper(
        &header[V5_SHA1_OFFSET..V5_SHA1_OFFSET + 20],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid v5 header with a chosen SHA1 at the right offset.
    fn v5_header(sha1: [u8; 20]) -> [u8; V5_HEADER_LEN] {
        let mut h = [0u8; V5_HEADER_LEN];
        h[0..8].copy_from_slice(CHD_MAGIC);
        h[8..12].copy_from_slice(&(V5_HEADER_LEN as u32).to_be_bytes());
        h[12..16].copy_from_slice(&5u32.to_be_bytes());
        h[V5_SHA1_OFFSET..V5_SHA1_OFFSET + 20].copy_from_slice(&sha1);
        h
    }

    #[test]
    fn parses_v5_sha1_at_offset_84() {
        // The real azumanga gdl-0018.chd internal SHA1 (verified against the DAT).
        let sha1 = [
            0x74, 0x9a, 0x56, 0xdd, 0x64, 0xab, 0x69, 0x7f, 0x17, 0x47, 0x0d, 0x8a, 0xe7, 0x97,
            0xf7, 0xe2, 0x0e, 0x9e, 0xb6, 0x46,
        ];
        let got = parse_v5_sha1(&v5_header(sha1)).unwrap();
        assert_eq!(got, "749A56DD64AB697F17470D8AE797F7E20E9EB646");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut h = v5_header([0u8; 20]);
        h[0] = b'X';
        assert!(parse_v5_sha1(&h).is_err());
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut h = v5_header([0u8; 20]);
        h[12..16].copy_from_slice(&4u32.to_be_bytes());
        let err = parse_v5_sha1(&h).unwrap_err().to_string();
        assert!(err.contains("version 4"), "got: {err}");
    }

    #[test]
    fn is_chd_path_matches_extension_case_insensitively() {
        assert!(is_chd_path(Path::new("game/disk.chd")));
        assert!(is_chd_path(Path::new("game/DISK.CHD")));
        assert!(!is_chd_path(Path::new("game/rom.zip")));
        assert!(!is_chd_path(Path::new("game/noext")));
    }
}
