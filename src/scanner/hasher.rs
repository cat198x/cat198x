//! File hashing utilities

use anyhow::Result;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::header;

/// Computed hashes for a file
#[derive(Debug, Clone, Default)]
pub struct FileHashes {
    pub sha1: String,
    pub md5: String,
    pub crc32: String,
    pub size: u64,
}

/// Result of hashing a file with header detection
#[derive(Debug, Clone)]
pub struct FileHashResult {
    /// Hashes of the full file (including any header)
    pub full: FileHashes,
    /// Hashes of headerless content (if header detected), None otherwise
    pub headerless: Option<FileHashes>,
    /// Detected header format, if any
    pub header: Option<header::RomHeader>,
}

/// Hash a file, computing SHA1, MD5, and CRC32 in a single pass
pub fn hash_file(path: &Path) -> Result<FileHashes> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut reader = std::io::BufReader::new(file);

    hash_reader(&mut reader, metadata.len())
}

/// Hash a file with automatic header detection
///
/// Returns both full-file hashes and headerless hashes (if a header is detected).
/// This allows matching against both headered and headerless DATs.
pub fn hash_file_with_header_detection(path: &Path) -> Result<FileHashResult> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    let mut reader = std::io::BufReader::new(file);

    // Get file extension for header detection context
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    // Read enough bytes for header detection (512 for SMC)
    let mut header_buf = [0u8; 512];
    let bytes_read = reader.read(&mut header_buf)?;
    let header_data = &header_buf[..bytes_read];

    // Detect header
    let detected_header = header::detect_header(header_data, file_size, &extension);

    // Reset to beginning and compute full-file hash
    reader.seek(SeekFrom::Start(0))?;
    let full_hashes = hash_reader(&mut reader, file_size)?;

    // If header detected, compute headerless hash
    let headerless_hashes = if let Some(ref h) = detected_header {
        let skip = h.skip_bytes as u64;
        if file_size > skip {
            reader.seek(SeekFrom::Start(skip))?;
            let headerless_size = file_size - skip;
            Some(hash_reader(&mut reader, headerless_size)?)
        } else {
            None
        }
    } else {
        None
    };

    Ok(FileHashResult {
        full: full_hashes,
        headerless: headerless_hashes,
        header: detected_header,
    })
}

/// Hash from a reader (for archive entries)
pub fn hash_reader<R: Read>(reader: &mut R, size: u64) -> Result<FileHashes> {
    use crc32fast::Hasher as Crc32Hasher;
    use md5::{Digest, Md5};
    use sha1::Sha1;

    let mut sha1_hasher = Sha1::new();
    let mut md5_hasher = Md5::new();
    let mut crc32 = Crc32Hasher::new();

    let mut buffer = [0u8; 64 * 1024]; // 64KB buffer

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let data = &buffer[..bytes_read];
        Digest::update(&mut sha1_hasher, data);
        Digest::update(&mut md5_hasher, data);
        crc32.update(data);
    }

    let sha1_result = sha1_hasher.finalize();
    let md5_result = md5_hasher.finalize();
    let crc32_result = crc32.finalize();

    Ok(FileHashes {
        sha1: format!("{:X}", sha1_result),
        md5: format!("{:X}", md5_result),
        crc32: format!("{:08X}", crc32_result),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_hash_known_content() {
        // Test with known content "test\n"
        // Verified with: echo -n "test\n" | sha1sum / md5sum / crc32
        let content = b"test\n";
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, content.len() as u64).unwrap();

        assert_eq!(hashes.sha1, "4E1243BD22C66E76C2BA9EDDC1F91394E57F9F83");
        assert_eq!(hashes.md5, "D8E8FCA2DC0F896FD7CB4CB0031BA249");
        assert_eq!(hashes.crc32, "3BB935C6");
        assert_eq!(hashes.size, 5);
    }

    #[test]
    fn test_empty_content() {
        let content: &[u8] = b"";
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, 0).unwrap();

        // Known hashes for empty content
        assert_eq!(hashes.sha1, "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
        assert_eq!(hashes.md5, "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(hashes.crc32, "00000000");
        assert_eq!(hashes.size, 0);
    }

    #[test]
    fn test_hash_hello_world() {
        // "Hello, World!" - common test vector
        let content = b"Hello, World!";
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, content.len() as u64).unwrap();

        assert_eq!(hashes.sha1, "0A0A9F2A6772942557AB5355D76AF442F8F65E01");
        assert_eq!(hashes.md5, "65A8E27D8879283831B664BD8B7F0AD4");
        assert_eq!(hashes.crc32, "EC4AC3D0");
        assert_eq!(hashes.size, 13);
    }

    #[test]
    fn test_hash_binary_content() {
        // Binary content with null bytes and high bytes
        let content: &[u8] = &[0x00, 0xFF, 0x80, 0x7F, 0x01, 0xFE];
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, content.len() as u64).unwrap();

        // Verify size and that hashes are computed (40-char SHA1, 32-char MD5, 8-char CRC32)
        assert_eq!(hashes.size, 6);
        assert_eq!(hashes.sha1.len(), 40);
        assert_eq!(hashes.md5.len(), 32);
        assert_eq!(hashes.crc32.len(), 8);
    }

    #[test]
    fn test_hash_large_content() {
        // Test content larger than buffer size (64KB)
        // Creates 128KB of repeated pattern
        let pattern: Vec<u8> = (0..256).map(|i| i as u8).collect();
        let content: Vec<u8> = pattern.iter().cycle().take(128 * 1024).copied().collect();
        let mut cursor = Cursor::new(&content);

        let hashes = hash_reader(&mut cursor, content.len() as u64).unwrap();

        assert_eq!(hashes.size, 128 * 1024);
        // Hashes are deterministic - same input always produces same output
        assert_eq!(hashes.sha1.len(), 40);
        assert_eq!(hashes.md5.len(), 32);
        assert_eq!(hashes.crc32.len(), 8);
    }

    #[test]
    fn test_hash_uppercase_hex_output() {
        // Verify all hash outputs are uppercase hex
        let content = b"abc";
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, content.len() as u64).unwrap();

        // SHA1 and MD5 should be uppercase
        assert!(hashes.sha1.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hashes.md5.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hashes.crc32.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify uppercase specifically (no lowercase a-f)
        assert!(!hashes.sha1.chars().any(|c| c.is_ascii_lowercase()));
        assert!(!hashes.md5.chars().any(|c| c.is_ascii_lowercase()));
        assert!(!hashes.crc32.chars().any(|c| c.is_ascii_lowercase()));
    }

    #[test]
    fn test_hash_abc() {
        // "abc" - well-known test vector with verified hashes
        let content = b"abc";
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, content.len() as u64).unwrap();

        assert_eq!(hashes.sha1, "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(hashes.md5, "900150983CD24FB0D6963F7D28E17F72");
        assert_eq!(hashes.crc32, "352441C2");
        assert_eq!(hashes.size, 3);
    }

    #[test]
    fn test_hash_deterministic() {
        // Hashing same content twice should produce identical results
        let content = b"deterministic test";

        let mut cursor1 = Cursor::new(content);
        let hashes1 = hash_reader(&mut cursor1, content.len() as u64).unwrap();

        let mut cursor2 = Cursor::new(content);
        let hashes2 = hash_reader(&mut cursor2, content.len() as u64).unwrap();

        assert_eq!(hashes1.sha1, hashes2.sha1);
        assert_eq!(hashes1.md5, hashes2.md5);
        assert_eq!(hashes1.crc32, hashes2.crc32);
        assert_eq!(hashes1.size, hashes2.size);
    }

    #[test]
    fn test_hash_different_content_different_hashes() {
        // Different content should produce different hashes
        let content1 = b"content one";
        let content2 = b"content two";

        let mut cursor1 = Cursor::new(content1);
        let hashes1 = hash_reader(&mut cursor1, content1.len() as u64).unwrap();

        let mut cursor2 = Cursor::new(content2);
        let hashes2 = hash_reader(&mut cursor2, content2.len() as u64).unwrap();

        assert_ne!(hashes1.sha1, hashes2.sha1);
        assert_ne!(hashes1.md5, hashes2.md5);
        assert_ne!(hashes1.crc32, hashes2.crc32);
    }

    #[test]
    fn test_crc32_zero_padded() {
        // CRC32 should be zero-padded to 8 characters
        // Empty content has CRC32 of 0, should display as "00000000"
        let content: &[u8] = b"";
        let mut cursor = Cursor::new(content);

        let hashes = hash_reader(&mut cursor, 0).unwrap();

        assert_eq!(hashes.crc32.len(), 8);
        assert_eq!(hashes.crc32, "00000000");
    }

    #[test]
    fn test_file_hashes_default() {
        // FileHashes implements Default
        let hashes = FileHashes::default();

        assert_eq!(hashes.sha1, "");
        assert_eq!(hashes.md5, "");
        assert_eq!(hashes.crc32, "");
        assert_eq!(hashes.size, 0);
    }
}
