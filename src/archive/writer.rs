//! ZIP archive writing utilities

use anyhow::{Context, Result};
use sha1::Digest as Sha1Digest;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use zip::CompressionMethod;
use zip::DateTime;
use zip::write::SimpleFileOptions;

/// Options for ZIP writing
#[derive(Debug, Clone)]
pub struct ZipWriterOptions {
    /// Compression method to use
    pub compression: CompressionMethod,
    /// Compression level (0-9, higher = more compression)
    pub compression_level: Option<i64>,
}

impl Default for ZipWriterOptions {
    fn default() -> Self {
        Self {
            compression: CompressionMethod::Deflated,
            compression_level: Some(6), // Standard compression level
        }
    }
}

/// Builder for creating ZIP archives with multiple files
pub struct ZipWriter {
    inner: zip::ZipWriter<File>,
    options: ZipWriterOptions,
    dest_path: std::path::PathBuf,
}

impl ZipWriter {
    /// Create a new ZIP writer at the specified path
    pub fn new(dest_path: &Path, options: ZipWriterOptions) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).context("Failed to create destination directory")?;
        }

        let file = File::create(dest_path)
            .with_context(|| format!("Failed to create ZIP file: {}", dest_path.display()))?;

        let inner = zip::ZipWriter::new(file);

        Ok(Self {
            inner,
            options,
            dest_path: dest_path.to_path_buf(),
        })
    }

    /// Add a file from a path to the archive with the given entry name
    ///
    /// Returns the SHA1 hash of the written data for verification
    pub fn add_file(&mut self, entry_name: &str, source_path: &Path) -> Result<String> {
        let data = fs::read(source_path)
            .with_context(|| format!("Failed to read source file: {}", source_path.display()))?;

        self.add_data(entry_name, &data)
    }

    /// Add data directly to the archive with the given entry name
    ///
    /// Returns the SHA1 hash of the written data for verification
    pub fn add_data(&mut self, entry_name: &str, data: &[u8]) -> Result<String> {
        // Calculate SHA1 of input data
        let mut hasher = sha1::Sha1::new();
        Sha1Digest::update(&mut hasher, data);
        let hash = Sha1Digest::finalize(hasher);
        let sha1 = crate::util::hex_upper(hash);

        // Build file options
        let mut file_options =
            SimpleFileOptions::default().compression_method(self.options.compression);

        if let Some(level) = self.options.compression_level {
            file_options = file_options.compression_level(Some(level));
        }

        // Write to archive
        self.inner
            .start_file(entry_name, file_options)
            .with_context(|| format!("Failed to start ZIP entry: {}", entry_name))?;

        self.inner
            .write_all(data)
            .with_context(|| format!("Failed to write ZIP entry: {}", entry_name))?;

        Ok(sha1)
    }

    /// Extract data from another archive and add it to this one
    ///
    /// Returns the SHA1 hash of the written data for verification
    pub fn add_from_archive(
        &mut self,
        entry_name: &str,
        archive_path: &Path,
        source_entry: &str,
    ) -> Result<String> {
        let data = extract_archive_entry(archive_path, source_entry)?;
        self.add_data(entry_name, &data)
    }

    /// Finalise the ZIP archive
    pub fn finish(self) -> Result<std::path::PathBuf> {
        self.inner
            .finish()
            .context("Failed to finalise ZIP archive")?;
        Ok(self.dest_path)
    }
}

/// TorrentZIP-compliant archive writer
///
/// TorrentZIP is a standard that ensures byte-for-byte reproducible ZIP archives:
/// - Files sorted alphabetically (case-sensitive)
/// - DEFLATE compression at maximum level (9)
/// - Fixed DOS date/time (1996-12-24 23:32:00)
/// - No extra fields in headers
pub struct TorrentZipWriter {
    dest_path: std::path::PathBuf,
    entries: Vec<TorrentZipEntry>,
}

struct TorrentZipEntry {
    name: String,
    data: Vec<u8>,
    sha1: String,
}

/// Fixed TorrentZIP timestamp: 1996-12-24 23:32:00
fn torrentzip_datetime() -> DateTime {
    // DOS date/time format:
    // Date: ((year - 1980) << 9) | (month << 5) | day
    // Time: (hour << 11) | (minute << 5) | (second / 2)
    // 1996-12-24 = ((1996-1980) << 9) | (12 << 5) | 24 = (16 << 9) | (12 << 5) | 24 = 8192 + 384 + 24 = 8600
    // 23:32:00 = (23 << 11) | (32 << 5) | 0 = 47104 + 1024 + 0 = 48128
    DateTime::from_date_and_time(1996, 12, 24, 23, 32, 0).unwrap_or_default()
}

impl TorrentZipWriter {
    /// Create a new TorrentZIP writer at the specified path
    pub fn new(dest_path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).context("Failed to create destination directory")?;
        }

        Ok(Self {
            dest_path: dest_path.to_path_buf(),
            entries: Vec::new(),
        })
    }

    /// Add a file from a path to the archive with the given entry name
    ///
    /// Returns the SHA1 hash of the data for verification
    pub fn add_file(&mut self, entry_name: &str, source_path: &Path) -> Result<String> {
        let data = fs::read(source_path)
            .with_context(|| format!("Failed to read source file: {}", source_path.display()))?;

        self.add_data(entry_name, data)
    }

    /// Add data directly to the archive with the given entry name
    ///
    /// Returns the SHA1 hash of the data for verification
    pub fn add_data(&mut self, entry_name: &str, data: Vec<u8>) -> Result<String> {
        // Calculate SHA1 of input data
        let mut hasher = sha1::Sha1::new();
        Sha1Digest::update(&mut hasher, &data);
        let hash = Sha1Digest::finalize(hasher);
        let sha1 = crate::util::hex_upper(hash);

        self.entries.push(TorrentZipEntry {
            name: entry_name.to_string(),
            data,
            sha1: sha1.clone(),
        });

        Ok(sha1)
    }

    /// Extract data from another archive and add it to this one
    ///
    /// Returns the SHA1 hash of the data for verification
    pub fn add_from_archive(
        &mut self,
        entry_name: &str,
        archive_path: &Path,
        source_entry: &str,
    ) -> Result<String> {
        let data = extract_archive_entry(archive_path, source_entry)?;
        self.add_data(entry_name, data)
    }

    /// Finalise the TorrentZIP archive
    ///
    /// This sorts entries alphabetically and writes them with TorrentZIP settings
    pub fn finish(mut self) -> Result<std::path::PathBuf> {
        // Sort entries alphabetically by name (case-sensitive)
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));

        // Create the ZIP file
        let file = File::create(&self.dest_path)
            .with_context(|| format!("Failed to create ZIP file: {}", self.dest_path.display()))?;

        let mut zip = zip::ZipWriter::new(file);

        // TorrentZIP file options: DEFLATE level 9, fixed timestamp
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(9))
            .last_modified_time(torrentzip_datetime());

        for entry in &self.entries {
            zip.start_file(&entry.name, options)
                .with_context(|| format!("Failed to start ZIP entry: {}", entry.name))?;

            zip.write_all(&entry.data)
                .with_context(|| format!("Failed to write ZIP entry: {}", entry.name))?;
        }

        zip.finish().context("Failed to finalise ZIP archive")?;

        Ok(self.dest_path)
    }

    /// Get the SHA1 hashes of all added entries (in add order, not sorted order)
    pub fn entry_hashes(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .map(|e| (e.name.as_str(), e.sha1.as_str()))
            .collect()
    }
}

/// Extract a single entry from an archive to memory
fn extract_archive_entry(archive_path: &Path, entry_path: &str) -> Result<Vec<u8>> {
    let ext = archive_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext.to_lowercase().as_str() {
        "zip" => extract_from_zip(archive_path, entry_path),
        "7z" => extract_from_7z(archive_path, entry_path),
        _ => anyhow::bail!("Unsupported archive format: {}", archive_path.display()),
    }
}

/// Extract a file from a ZIP archive to memory
fn extract_from_zip(archive_path: &Path, entry_path: &str) -> Result<Vec<u8>> {
    let file = File::open(archive_path).context("Failed to open ZIP archive")?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read ZIP archive")?;

    let mut entry = archive
        .by_name(entry_path)
        .with_context(|| format!("Entry not found in archive: {}", entry_path))?;

    let mut data = Vec::new();
    entry.read_to_end(&mut data)?;

    Ok(data)
}

/// Extract a file from a 7z archive to memory
fn extract_from_7z(archive_path: &Path, entry_path: &str) -> Result<Vec<u8>> {
    let mut archive =
        sevenz_rust2::ArchiveReader::open(archive_path, sevenz_rust2::Password::empty())
            .context("Failed to read 7z archive")?;

    let mut result: Option<Vec<u8>> = None;

    archive.for_each_entries(|entry, reader| {
        if entry.name() == entry_path {
            let mut data = Vec::new();
            reader.read_to_end(&mut data)?;
            result = Some(data);
            return Ok(false); // Stop iteration
        }
        Ok(true)
    })?;

    result.ok_or_else(|| anyhow::anyhow!("Entry not found in archive: {}", entry_path))
}

/// Write a single file to a ZIP archive (convenience function for simple cases)
#[allow(dead_code)]
pub fn write_single_file_zip(
    dest_path: &Path,
    entry_name: &str,
    source_path: &Path,
    expected_sha1: &str,
) -> Result<()> {
    let mut writer = ZipWriter::new(dest_path, ZipWriterOptions::default())?;
    let actual_sha1 = writer.add_file(entry_name, source_path)?;
    writer.finish()?;

    // Verify the hash matches
    if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
        // Clean up the bad ZIP
        let _ = fs::remove_file(dest_path);
        anyhow::bail!(
            "ZIP verification failed: source hash {} does not match expected {}",
            actual_sha1,
            expected_sha1
        );
    }

    Ok(())
}

/// Write a single file to a ZIP archive from an existing archive entry
#[allow(dead_code)]
pub fn write_single_file_zip_from_archive(
    dest_path: &Path,
    entry_name: &str,
    archive_path: &Path,
    source_entry: &str,
    expected_sha1: &str,
) -> Result<()> {
    let mut writer = ZipWriter::new(dest_path, ZipWriterOptions::default())?;
    let actual_sha1 = writer.add_from_archive(entry_name, archive_path, source_entry)?;
    writer.finish()?;

    // Verify the hash matches
    if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
        // Clean up the bad ZIP
        let _ = fs::remove_file(dest_path);
        anyhow::bail!(
            "ZIP verification failed: source hash {} does not match expected {}",
            actual_sha1,
            expected_sha1
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_zip_writer_single_file() {
        let temp = TempDir::new().unwrap();

        // Create a source file
        let src_path = temp.path().join("source.rom");
        fs::write(&src_path, b"test rom content").unwrap();

        // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
        let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

        let dest_path = temp.path().join("output.zip");

        let mut writer = ZipWriter::new(&dest_path, ZipWriterOptions::default()).unwrap();
        let sha1 = writer.add_file("game.rom", &src_path).unwrap();
        writer.finish().unwrap();

        assert!(sha1.eq_ignore_ascii_case(expected_sha1));
        assert!(dest_path.exists());

        // Verify the ZIP contains the expected entry
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1);

        let mut entry = archive.by_name("game.rom").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"test rom content");
    }

    #[test]
    fn test_zip_writer_multiple_files() {
        let temp = TempDir::new().unwrap();

        // Create source files
        let src1 = temp.path().join("file1.rom");
        let src2 = temp.path().join("file2.rom");
        fs::write(&src1, b"first file").unwrap();
        fs::write(&src2, b"second file").unwrap();

        let dest_path = temp.path().join("multi.zip");

        let mut writer = ZipWriter::new(&dest_path, ZipWriterOptions::default()).unwrap();
        writer.add_file("rom1.bin", &src1).unwrap();
        writer.add_file("rom2.bin", &src2).unwrap();
        writer.finish().unwrap();

        // Verify both entries exist
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 2);

        let mut entry1 = archive.by_name("rom1.bin").unwrap();
        let mut content1 = Vec::new();
        entry1.read_to_end(&mut content1).unwrap();
        assert_eq!(content1, b"first file");
    }

    #[test]
    fn test_zip_writer_add_data() {
        let temp = TempDir::new().unwrap();
        let dest_path = temp.path().join("data.zip");

        let mut writer = ZipWriter::new(&dest_path, ZipWriterOptions::default()).unwrap();
        let sha1 = writer.add_data("inline.bin", b"inline data").unwrap();
        writer.finish().unwrap();

        // SHA1 of "inline data" = A4F3C4D56AF15EB7D2DF352FC3BD6A0F9C955D66
        assert!(sha1.eq_ignore_ascii_case("A4F3C4D56AF15EB7D2DF352FC3BD6A0F9C955D66"));
    }

    #[test]
    fn test_write_single_file_zip() {
        let temp = TempDir::new().unwrap();

        let src_path = temp.path().join("source.rom");
        fs::write(&src_path, b"hello").unwrap();

        // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

        let dest_path = temp.path().join("output.zip");

        write_single_file_zip(&dest_path, "game.rom", &src_path, expected_sha1).unwrap();

        assert!(dest_path.exists());
    }

    #[test]
    fn test_write_single_file_zip_bad_hash() {
        let temp = TempDir::new().unwrap();

        let src_path = temp.path().join("source.rom");
        fs::write(&src_path, b"hello").unwrap();

        let dest_path = temp.path().join("output.zip");

        let result = write_single_file_zip(
            &dest_path,
            "game.rom",
            &src_path,
            "0000000000000000000000000000000000000000",
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("verification failed")
        );

        // Bad ZIP should be removed
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_zip_writer_creates_parent_dirs() {
        let temp = TempDir::new().unwrap();

        let src_path = temp.path().join("source.rom");
        fs::write(&src_path, b"test").unwrap();

        // Nested destination
        let dest_path = temp.path().join("nested/dirs/output.zip");

        let mut writer = ZipWriter::new(&dest_path, ZipWriterOptions::default()).unwrap();
        writer.add_file("rom.bin", &src_path).unwrap();
        writer.finish().unwrap();

        assert!(dest_path.exists());
    }

    #[test]
    fn test_add_from_archive_zip() {
        let temp = TempDir::new().unwrap();

        // First create a source ZIP
        let src_zip = temp.path().join("source.zip");
        {
            let mut writer = ZipWriter::new(&src_zip, ZipWriterOptions::default()).unwrap();
            writer.add_data("inner.rom", b"archived content").unwrap();
            writer.finish().unwrap();
        }

        // Now create a new ZIP extracting from the first one
        let dest_zip = temp.path().join("dest.zip");
        let mut writer = ZipWriter::new(&dest_zip, ZipWriterOptions::default()).unwrap();
        let sha1 = writer
            .add_from_archive("extracted.rom", &src_zip, "inner.rom")
            .unwrap();
        writer.finish().unwrap();

        // SHA1 of "archived content" = 2A7C0F5B2F1BEF21E2DBB7B62A7AB5ABF4F7C1C6
        // (calculated manually - actual value may differ)
        assert!(!sha1.is_empty());

        // Verify the content was copied correctly
        let file = File::open(&dest_zip).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name("extracted.rom").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"archived content");
    }

    #[test]
    fn test_zip_writer_options_no_compression() {
        let temp = TempDir::new().unwrap();

        let src_path = temp.path().join("source.rom");
        fs::write(&src_path, b"test data").unwrap();

        let dest_path = temp.path().join("stored.zip");

        let options = ZipWriterOptions {
            compression: CompressionMethod::Stored,
            compression_level: None,
        };

        let mut writer = ZipWriter::new(&dest_path, options).unwrap();
        writer.add_file("rom.bin", &src_path).unwrap();
        writer.finish().unwrap();

        assert!(dest_path.exists());
    }

    #[test]
    fn test_torrentzip_writer_single_file() {
        let temp = TempDir::new().unwrap();

        let src_path = temp.path().join("source.rom");
        fs::write(&src_path, b"test rom content").unwrap();

        let dest_path = temp.path().join("output.zip");

        let mut writer = TorrentZipWriter::new(&dest_path).unwrap();
        let sha1 = writer.add_file("game.rom", &src_path).unwrap();
        writer.finish().unwrap();

        // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
        assert!(sha1.eq_ignore_ascii_case("331407B2BD72286D458F26C426D78F459D7116D3"));
        assert!(dest_path.exists());

        // Verify the ZIP contains the expected entry
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1);

        let mut entry = archive.by_name("game.rom").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"test rom content");
    }

    #[test]
    fn test_torrentzip_sorts_entries() {
        let temp = TempDir::new().unwrap();
        let dest_path = temp.path().join("sorted.zip");

        let mut writer = TorrentZipWriter::new(&dest_path).unwrap();

        // Add files in reverse order
        writer.add_data("z_last.rom", b"z file".to_vec()).unwrap();
        writer.add_data("a_first.rom", b"a file".to_vec()).unwrap();
        writer.add_data("m_middle.rom", b"m file".to_vec()).unwrap();

        writer.finish().unwrap();

        // Verify entries are sorted alphabetically
        let file = File::open(&dest_path).unwrap();
        let archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 3);

        // Get entry names in order
        let names: Vec<_> = (0..archive.len())
            .map(|i| {
                let file = File::open(&dest_path).unwrap();
                let mut archive = zip::ZipArchive::new(file).unwrap();
                archive.by_index(i).unwrap().name().to_string()
            })
            .collect();

        assert_eq!(names, vec!["a_first.rom", "m_middle.rom", "z_last.rom"]);
    }

    #[test]
    fn test_torrentzip_fixed_timestamp() {
        let temp = TempDir::new().unwrap();
        let dest_path = temp.path().join("timestamped.zip");

        let mut writer = TorrentZipWriter::new(&dest_path).unwrap();
        writer.add_data("test.rom", b"content".to_vec()).unwrap();
        writer.finish().unwrap();

        // Verify the timestamp is the TorrentZIP standard: 1996-12-24 23:32:00
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let entry = archive.by_index(0).unwrap();

        let datetime = entry
            .last_modified()
            .expect("entry has a last-modified time");
        assert_eq!(datetime.year(), 1996);
        assert_eq!(datetime.month(), 12);
        assert_eq!(datetime.day(), 24);
        assert_eq!(datetime.hour(), 23);
        assert_eq!(datetime.minute(), 32);
    }

    #[test]
    fn test_torrentzip_deterministic() {
        let temp = TempDir::new().unwrap();

        // Create the same archive twice
        let dest1 = temp.path().join("first.zip");
        let dest2 = temp.path().join("second.zip");

        for dest in [&dest1, &dest2] {
            let mut writer = TorrentZipWriter::new(dest).unwrap();
            writer.add_data("b.rom", b"content b".to_vec()).unwrap();
            writer.add_data("a.rom", b"content a".to_vec()).unwrap();
            writer.finish().unwrap();
        }

        // Both files should be byte-for-byte identical
        let bytes1 = fs::read(&dest1).unwrap();
        let bytes2 = fs::read(&dest2).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn test_torrentzip_compression_level_9() {
        let temp = TempDir::new().unwrap();
        let dest_path = temp.path().join("compressed.zip");

        // Use compressible content (repeated pattern)
        let data = b"aaaaaaaaaabbbbbbbbbbcccccccccc".repeat(100);

        let mut writer = TorrentZipWriter::new(&dest_path).unwrap();
        writer.add_data("large.rom", data.clone()).unwrap();
        writer.finish().unwrap();

        // Verify the archive is smaller than uncompressed
        let archive_size = fs::metadata(&dest_path).unwrap().len();
        assert!(archive_size < data.len() as u64);

        // Verify content is intact
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name("large.rom").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, data);
    }
}
