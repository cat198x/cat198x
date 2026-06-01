//! Shared utility functions used across the codebase

use anyhow::{Context, Result};
use sha1::Digest;
use std::path::Path;

/// Truncate a path string for display, keeping the end visible
///
/// If the path is longer than `max_len`, it's truncated with "..." at the start.
///
/// # Examples
/// ```
/// use cat198x::util::truncate_path;
/// assert_eq!(truncate_path("short.txt", 20), "short.txt");
/// assert_eq!(truncate_path("very/long/path/file.txt", 15), "...ath/file.txt");
/// ```
pub fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        format!("...{}", &path[path.len() - max_len + 3..])
    }
}

/// Verify a file has the expected SHA1 hash
///
/// Returns `Ok(true)` if the hash matches, `Ok(false)` if it doesn't,
/// or an error if the file couldn't be read.
pub fn verify_sha1(path: &Path, expected: &str) -> Result<bool> {
    let contents = std::fs::read(path).context("Failed to read file for verification")?;
    let mut hasher = sha1::Sha1::new();
    hasher.update(&contents);
    let hash = hasher.finalize();
    let actual = hex_upper(hash);

    Ok(actual.eq_ignore_ascii_case(expected))
}

/// Format bytes as an uppercase hex string with no separators.
///
/// This is the canonical hash representation used throughout the catalogue
/// (SHA-1, MD5, CRC32). RustCrypto 0.11 changed digest outputs to a type that
/// no longer implements `UpperHex`, so the formatting lives here instead of a
/// `format!("{:X}", digest)`.
pub fn hex_upper(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let bytes = bytes.as_ref();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{:02X}", b).expect("writing to a String never fails");
    }
    s
}

/// Format bytes as a lowercase hex string with no separators.
pub fn hex_lower(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let bytes = bytes.as_ref();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{:02x}", b).expect("writing to a String never fails");
    }
    s
}

/// Format a byte count as a human-readable string
///
/// # Examples
/// ```
/// use cat198x::util::format_bytes;
/// assert_eq!(format_bytes(0), "0 bytes");
/// assert_eq!(format_bytes(1024), "1.00 KB");
/// assert_eq!(format_bytes(1048576), "1.00 MB");
/// ```
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_truncate_path_short() {
        assert_eq!(truncate_path("short.txt", 20), "short.txt");
    }

    #[test]
    fn test_truncate_path_exact() {
        assert_eq!(
            truncate_path("exactly20chars.txt!!", 20),
            "exactly20chars.txt!!"
        );
    }

    #[test]
    fn test_truncate_path_long() {
        let long = "very/long/path/to/some/file.txt";
        let truncated = truncate_path(long, 15);
        assert!(truncated.starts_with("..."));
        assert_eq!(truncated.len(), 15);
    }

    #[test]
    fn test_verify_sha1_correct() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.rom");

        // "hello" has SHA1 = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello").unwrap();

        assert!(verify_sha1(&file_path, "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D").unwrap());
    }

    #[test]
    fn test_verify_sha1_incorrect() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.rom");

        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello").unwrap();

        assert!(!verify_sha1(&file_path, "0000000000000000000000000000000000000000").unwrap());
    }

    #[test]
    fn test_verify_sha1_case_insensitive() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.rom");

        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello").unwrap();

        // Lowercase hash should also match
        assert!(verify_sha1(&file_path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d").unwrap());
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
    }
}
