//! Plan execution engine: the file operations that carry out a plan.
//!
//! These functions perform the actual copies, moves, repacks, extractions, and
//! rollback moves. Each writes to its destination, verifies the result against
//! the expected SHA-1, and only then removes any source — so an interrupted or
//! corrupt operation can never lose the original ROM.
//!
//! The engine holds no CLI or progress-reporting concerns: it takes paths and
//! plan types and returns `Result`s. That keeps it reusable — the `apply`
//! command drives it here, and other 198x tools (e.g. Forge198x) can call the
//! same primitives directly.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::plan::{OperationKind, OperationStatus, Plan, SourceRef};
use crate::util::{format_bytes, verify_sha1};

/// Execute a rollback move operation
pub fn execute_rollback_move(source: &str, dest: &str, expected_sha1: &str) -> Result<()> {
    let source_path = Path::new(source);
    let dest_path = Path::new(dest);

    // Verify source file has expected hash
    if source_path.exists() {
        if !verify_sha1(source_path, expected_sha1)? {
            anyhow::bail!("Source file hash mismatch - cannot safely rollback");
        }

        // Create destination directory if needed
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Move the file
        fs::rename(source_path, dest_path).or_else(|_| {
            // Cross-device: copy, re-verify the copy, flush it to disk, and
            // only then delete the source — so a corrupt or unflushed copy
            // can't lose the very file we're trying to restore.
            fs::copy(source_path, dest_path)?;
            if !verify_sha1(dest_path, expected_sha1)? {
                let _ = fs::remove_file(dest_path);
                anyhow::bail!("Rollback copy verification failed for {}", dest);
            }
            std::fs::File::open(dest_path)
                .and_then(|f| f.sync_all())
                .with_context(|| format!("Failed to flush restored file: {}", dest))?;
            fs::remove_file(source_path)?;
            Ok::<_, anyhow::Error>(())
        })?;
    } else {
        anyhow::bail!("Source file not found: {}", source);
    }

    Ok(())
}

/// Execute a copy operation from source to destination with verification
///
/// If dest_path ends with .zip, the file will be written into a ZIP archive.
/// The entry name inside the ZIP is derived from the dest_path filename without .zip extension.
pub fn execute_copy(
    source_path: &str,
    archive_path: Option<&str>,
    dest_path: &str,
    expected_sha1: &str,
) -> Result<()> {
    let dest = Path::new(dest_path);

    // Check if we're writing to a ZIP archive
    if dest_path.to_lowercase().ends_with(".zip") {
        return execute_copy_to_zip(source_path, archive_path, dest_path, expected_sha1);
    }

    // Create destination directory if needed
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create destination directory")?;
    }

    // Perform the copy/extraction
    match archive_path {
        Some(entry_path) => {
            extract_from_archive(source_path, entry_path, dest_path)?;
        }
        None => {
            fs::copy(source_path, dest_path).context("Failed to copy file")?;
        }
    }

    // Verify the written file matches expected hash
    if !verify_sha1(dest, expected_sha1)? {
        // Remove the bad file
        let _ = fs::remove_file(dest);
        anyhow::bail!(
            "Verification failed: written file hash does not match expected SHA1 {}",
            expected_sha1
        );
    }

    Ok(())
}

/// Execute a move operation from source to destination with verification
///
/// A move is a copy followed by deletion of the source.
/// If the source is inside an archive, we only copy (can't delete from archives).
pub fn execute_move(
    source_path: &str,
    archive_path: Option<&str>,
    dest_path: &str,
    expected_sha1: &str,
) -> Result<()> {
    // First, copy the file to the destination (reuse existing copy logic)
    execute_copy(source_path, archive_path, dest_path, expected_sha1)?;

    // If source is inside an archive, we can't delete it - just return success
    if archive_path.is_some() {
        // Note: In the future, we could track these for archive cleanup
        return Ok(());
    }

    // Flush the verified destination to disk before deleting the source, so a
    // power loss in this window can't lose both copies of the ROM. Verification
    // above reads back through the page cache, which is not a durability
    // guarantee on its own.
    std::fs::File::open(dest_path)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("Failed to flush destination before delete: {}", dest_path))?;

    // Delete the source file (only for loose files)
    let source = Path::new(source_path);
    if source.exists() {
        fs::remove_file(source).with_context(|| {
            format!("Failed to delete source file after move: {}", source_path)
        })?;
    }

    Ok(())
}

/// Execute a copy operation where the destination is a ZIP archive
///
/// The entry name inside the ZIP is derived from the source file name.
fn execute_copy_to_zip(
    source_path: &str,
    archive_path: Option<&str>,
    dest_path: &str,
    expected_sha1: &str,
) -> Result<()> {
    use crate::archive::{ZipWriter, ZipWriterOptions};

    let dest = Path::new(dest_path);

    // Derive entry name from source filename
    let entry_name = Path::new(source_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("rom.bin");

    // If source is inside an archive, use the inner filename instead
    let entry_name = archive_path
        .map(|p| {
            Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(entry_name)
        })
        .unwrap_or(entry_name);

    // Create destination directory if needed
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create destination directory")?;
    }

    let mut writer = ZipWriter::new(dest, ZipWriterOptions::default())?;

    let actual_sha1 = match archive_path {
        Some(entry_path) => {
            writer.add_from_archive(entry_name, Path::new(source_path), entry_path)?
        }
        None => writer.add_file(entry_name, Path::new(source_path))?,
    };

    writer.finish()?;

    // Verify the hash matches
    if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
        // Clean up the bad ZIP
        let _ = fs::remove_file(dest);
        anyhow::bail!(
            "Verification failed: source hash {} does not match expected SHA1 {}",
            actual_sha1,
            expected_sha1
        );
    }

    Ok(())
}

/// Execute a repack operation - combine multiple source files into a single archive
///
/// Each source file is verified against its expected SHA1 before being added.
/// Supports "zip" and "torrentzip" formats.
pub fn execute_repack(sources: &[SourceRef], dest_path: &str, format: &str) -> Result<()> {
    let dest = Path::new(dest_path);

    // Create destination directory if needed
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create destination directory")?;
    }

    match format {
        "zip" => execute_repack_zip(sources, dest),
        "torrentzip" => execute_repack_torrentzip(sources, dest),
        _ => anyhow::bail!("Unsupported repack format: {} (use 'zip' or 'torrentzip')", format),
    }
}

/// Repack using standard ZIP format
fn execute_repack_zip(sources: &[SourceRef], dest: &Path) -> Result<()> {
    use crate::archive::{ZipWriter, ZipWriterOptions};

    let mut writer = ZipWriter::new(dest, ZipWriterOptions::default())?;
    let mut verification_errors = Vec::new();

    for source in sources {
        let entry_name = get_entry_name(source);
        let actual_sha1 = add_source_to_zip(&mut writer, source, entry_name)?;

        if !actual_sha1.eq_ignore_ascii_case(&source.sha1) {
            verification_errors.push(format!(
                "{}: expected {}, got {}",
                entry_name, source.sha1, actual_sha1
            ));
        }
    }

    if !verification_errors.is_empty() {
        drop(writer);
        let _ = fs::remove_file(dest);
        anyhow::bail!(
            "Repack verification failed for {} file(s):\n  {}",
            verification_errors.len(),
            verification_errors.join("\n  ")
        );
    }

    writer.finish()?;
    Ok(())
}

/// Repack using TorrentZIP format (deterministic, sorted, max compression)
fn execute_repack_torrentzip(sources: &[SourceRef], dest: &Path) -> Result<()> {
    use crate::archive::TorrentZipWriter;

    let mut writer = TorrentZipWriter::new(dest)?;
    let mut verification_errors = Vec::new();

    for source in sources {
        let entry_name = get_entry_name(source);
        let actual_sha1 = add_source_to_torrentzip(&mut writer, source, entry_name)?;

        if !actual_sha1.eq_ignore_ascii_case(&source.sha1) {
            verification_errors.push(format!(
                "{}: expected {}, got {}",
                entry_name, source.sha1, actual_sha1
            ));
        }
    }

    if !verification_errors.is_empty() {
        // TorrentZipWriter buffers in memory, so no file created yet
        let _ = fs::remove_file(dest);
        anyhow::bail!(
            "Repack verification failed for {} file(s):\n  {}",
            verification_errors.len(),
            verification_errors.join("\n  ")
        );
    }

    writer.finish()?;
    Ok(())
}

/// Get the entry name for a source file
fn get_entry_name(source: &SourceRef) -> &str {
    source
        .archive_path
        .as_ref()
        .and_then(|p| Path::new(p).file_name())
        .or_else(|| Path::new(&source.path).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("rom.bin")
}

/// Add a source to a ZipWriter
fn add_source_to_zip(
    writer: &mut crate::archive::ZipWriter,
    source: &SourceRef,
    entry_name: &str,
) -> Result<String> {
    match &source.archive_path {
        Some(archive_entry) => {
            writer.add_from_archive(entry_name, Path::new(&source.path), archive_entry)
        }
        None => writer.add_file(entry_name, Path::new(&source.path)),
    }
}

/// Add a source to a TorrentZipWriter
fn add_source_to_torrentzip(
    writer: &mut crate::archive::TorrentZipWriter,
    source: &SourceRef,
    entry_name: &str,
) -> Result<String> {
    match &source.archive_path {
        Some(archive_entry) => {
            writer.add_from_archive(entry_name, Path::new(&source.path), archive_entry)
        }
        None => writer.add_file(entry_name, Path::new(&source.path)),
    }
}

/// Extract a file from an archive to destination
fn extract_from_archive(archive_path: &str, entry_path: &str, dest_path: &str) -> Result<()> {
    let archive = Path::new(archive_path);

    match archive.extension().and_then(|e| e.to_str()) {
        Some("zip") => extract_from_zip(archive_path, entry_path, dest_path),
        Some("7z") => extract_from_7z(archive_path, entry_path, dest_path),
        _ => anyhow::bail!("Unsupported archive format: {}", archive_path),
    }
}

/// Extract a file from a ZIP archive
fn extract_from_zip(archive_path: &str, entry_path: &str, dest_path: &str) -> Result<()> {
    let file = fs::File::open(archive_path).context("Failed to open ZIP archive")?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read ZIP archive")?;

    let mut entry = archive
        .by_name(entry_path)
        .with_context(|| format!("Entry not found in archive: {}", entry_path))?;

    let dest = Path::new(dest_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut dest_file = fs::File::create(dest_path).context("Failed to create destination file")?;
    std::io::copy(&mut entry, &mut dest_file).context("Failed to extract file")?;

    Ok(())
}

/// Extract a file from a 7z archive
fn extract_from_7z(archive_path: &str, entry_path: &str, dest_path: &str) -> Result<()> {
    use sevenz_rust2::ArchiveReader;

    let archive = ArchiveReader::open(archive_path, sevenz_rust2::Password::empty())
        .context("Failed to read 7z archive")?;

    // Find the entry
    let mut found = false;
    for entry in archive.archive().files.iter() {
        if entry.name() == entry_path {
            found = true;
            break;
        }
    }

    if !found {
        anyhow::bail!("Entry not found in archive: {}", entry_path);
    }

    // Extract to temp then move
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    sevenz_rust2::decompress_file(archive_path, temp_dir.path())
        .context("Failed to decompress 7z archive")?;

    let extracted = temp_dir.path().join(entry_path);
    if !extracted.exists() {
        anyhow::bail!("Failed to extract entry: {}", entry_path);
    }

    let dest = Path::new(dest_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(&extracted, dest_path).context("Failed to copy extracted file")?;

    Ok(())
}


/// Check if there's enough disk space for all planned operations
///
/// Groups operations by destination filesystem mount point and checks
/// available space against total bytes to write.
pub fn check_disk_space(plan: &Plan) -> Result<()> {
    // Group bytes needed by destination directory (filesystem mount approximation)
    let mut bytes_by_dest: HashMap<String, u64> = HashMap::new();

    for op in &plan.operations {
        if op.status != OperationStatus::Pending {
            continue;
        }

        match &op.kind {
            OperationKind::Copy { dest, size, .. } | OperationKind::Move { dest, size, .. } => {
                // Get the parent directory as a rough mount point indicator
                let dest_dir = Path::new(dest)
                    .parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or("/")
                    .to_string();

                *bytes_by_dest.entry(dest_dir).or_insert(0) += size;
            }
            OperationKind::Repack { sources, dest, .. } => {
                // For repack, estimate size as sum of source sizes
                let total_size: u64 = sources.iter().filter_map(|s| {
                    // Try to get file size from source path
                    fs::metadata(&s.path).ok().map(|m| m.len())
                }).sum();

                let dest_dir = Path::new(dest)
                    .parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or("/")
                    .to_string();

                *bytes_by_dest.entry(dest_dir).or_insert(0) += total_size;
            }
            OperationKind::Delete { .. } => {
                // Deletes free space, don't count
            }
            OperationKind::Quarantine { size, .. } => {
                // Quarantine moves to data_dir/quarantine, need space there
                // We'll approximate by using a standard quarantine path
                // In practice this should be the data_dir but we don't have it here
                // For now, just account for the size in a general bucket
                *bytes_by_dest.entry("quarantine".to_string()).or_insert(0) += size;
            }
        }
    }

    // Check available space for each destination
    for (dest_dir, bytes_needed) in &bytes_by_dest {
        let available = get_available_space(dest_dir)?;

        // Add 10% safety margin
        let bytes_with_margin = (*bytes_needed as f64 * 1.1) as u64;

        if available < bytes_with_margin {
            anyhow::bail!(
                "Insufficient space in '{}': need {} (with 10% margin), have {}",
                dest_dir,
                format_bytes(bytes_with_margin),
                format_bytes(available)
            );
        }
    }

    Ok(())
}

/// Get available disk space for a path (in bytes)
fn get_available_space(path: &str) -> Result<u64> {
    // Find an existing parent directory to stat — the destination itself may
    // not exist yet (we're about to create it).
    let mut check_path = Path::new(path).to_path_buf();
    while !check_path.exists() {
        check_path = match check_path.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
    }

    // fs4 wraps statvfs / GetDiskFreeSpaceExW, returning the space available to
    // non-privileged users (matching the old f_bavail-based result) with no
    // unsafe FFI on our side, so the crate keeps unsafe_code = "forbid".
    fs4::available_space(&check_path)
        .with_context(|| format!("Failed to get disk space for '{}'", path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{truncate_path, verify_sha1};
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_truncate_path_short() {
        assert_eq!(truncate_path("/short/path", 50), "/short/path");
    }

    #[test]
    fn test_truncate_path_long() {
        let long = "/very/long/path/that/exceeds/the/maximum/length/allowed";
        let truncated = truncate_path(long, 30);
        assert!(truncated.starts_with("..."));
        assert_eq!(truncated.len(), 30);
    }

    #[test]
    fn test_execute_copy_loose_file() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("dest/output.rom");

        // Create source file
        let mut src = fs::File::create(&src_path).unwrap();
        src.write_all(b"test rom content").unwrap();

        // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
        let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

        // Execute copy with verification
        execute_copy(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            expected_sha1,
        )
        .unwrap();

        // Verify destination exists with correct content
        let content = fs::read(&dest_path).unwrap();
        assert_eq!(content, b"test rom content");
    }

    #[test]
    fn test_execute_copy_verification_fails_on_bad_hash() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("dest/output.rom");

        // Create source file
        let mut src = fs::File::create(&src_path).unwrap();
        src.write_all(b"test rom content").unwrap();

        // Wrong SHA1
        let wrong_sha1 = "0000000000000000000000000000000000000000";

        // Execute copy should fail verification
        let result = execute_copy(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            wrong_sha1,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Verification failed"));

        // Bad file should be removed
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_verify_sha1() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.rom");

        // Create file with known content
        fs::write(&file_path, b"hello").unwrap();

        // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        assert!(verify_sha1(&file_path, "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D").unwrap());
        assert!(!verify_sha1(&file_path, "0000000000000000000000000000000000000000").unwrap());
    }

    #[test]
    fn test_execute_copy_to_zip_output() {
        use std::io::Read;

        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("output.zip");

        // Create source file
        let mut src = fs::File::create(&src_path).unwrap();
        src.write_all(b"test rom content").unwrap();

        // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
        let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

        // Execute copy to ZIP destination
        execute_copy(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            expected_sha1,
        )
        .unwrap();

        // Verify ZIP was created
        assert!(dest_path.exists());

        // Verify ZIP contains the file with correct content
        let file = fs::File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1);

        let mut entry = archive.by_name("source.rom").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"test rom content");
    }

    #[test]
    fn test_execute_copy_to_zip_bad_hash() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("output.zip");

        // Create source file
        let mut src = fs::File::create(&src_path).unwrap();
        src.write_all(b"test rom content").unwrap();

        // Wrong SHA1
        let wrong_sha1 = "0000000000000000000000000000000000000000";

        // Execute copy should fail verification
        let result = execute_copy(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            wrong_sha1,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Verification failed"));

        // Bad ZIP should be removed
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_execute_copy_to_zip_creates_parent_dirs() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("nested/dirs/output.zip");

        // Create source file
        fs::write(&src_path, b"hello").unwrap();

        // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

        // Execute copy to nested ZIP destination
        execute_copy(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            expected_sha1,
        )
        .unwrap();

        // Verify ZIP was created in nested directory
        assert!(dest_path.exists());
    }

    #[test]
    fn test_execute_repack_multiple_files() {
        use crate::plan::SourceRef;
        use std::io::Read;

        let temp = TempDir::new().unwrap();

        // Create source files
        let src1 = temp.path().join("cpu.rom");
        let src2 = temp.path().join("gfx.rom");
        fs::write(&src1, b"cpu data").unwrap();
        fs::write(&src2, b"graphics data").unwrap();

        // SHA1 of "cpu data" = 7D3A7E2E4F5B8C1D9E0F1A2B3C4D5E6F7A8B9C0D (wrong - calculate real)
        // SHA1 of "graphics data" = ... (calculate real)
        let dest_path = temp.path().join("game.zip");

        let sources = vec![
            SourceRef {
                path: src1.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "76218C22675632AEF6A27578DD0A2C6471D995D5".to_string(), // SHA1 of "cpu data"
            },
            SourceRef {
                path: src2.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "75BF07C00E138F33E12904F575641F0C06CBB838".to_string(), // SHA1 of "graphics data"
            },
        ];

        execute_repack(&sources, dest_path.to_str().unwrap(), "zip").unwrap();

        // Verify ZIP was created with both files
        assert!(dest_path.exists());

        let file = fs::File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 2);

        // Verify content of first file
        let mut entry1 = archive.by_name("cpu.rom").unwrap();
        let mut content1 = Vec::new();
        entry1.read_to_end(&mut content1).unwrap();
        assert_eq!(content1, b"cpu data");

        // Verify content of second file
        drop(entry1);
        let mut entry2 = archive.by_name("gfx.rom").unwrap();
        let mut content2 = Vec::new();
        entry2.read_to_end(&mut content2).unwrap();
        assert_eq!(content2, b"graphics data");
    }

    #[test]
    fn test_execute_repack_verification_failure() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();

        // Create source files
        let src1 = temp.path().join("good.rom");
        let src2 = temp.path().join("bad.rom");
        fs::write(&src1, b"good").unwrap();
        fs::write(&src2, b"bad").unwrap();

        let dest_path = temp.path().join("game.zip");

        let sources = vec![
            SourceRef {
                path: src1.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "FC19318DD13128CE14344D066510A982269C241B".to_string(), // SHA1 of "good"
            },
            SourceRef {
                path: src2.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "0000000000000000000000000000000000000000".to_string(), // Wrong hash
            },
        ];

        let result = execute_repack(&sources, dest_path.to_str().unwrap(), "zip");

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("verification failed"));

        // Bad ZIP should be removed
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_execute_repack_unsupported_format() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();
        let src = temp.path().join("file.rom");
        fs::write(&src, b"data").unwrap();

        let dest_path = temp.path().join("game.7z");

        let sources = vec![SourceRef {
            path: src.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "A17C9AAA61E80A1BF71D0D850AF4E5BAA9800BBD".to_string(), // SHA1 of "data"
        }];

        let result = execute_repack(&sources, dest_path.to_str().unwrap(), "7z");

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unsupported repack format"));
    }

    #[test]
    fn test_execute_repack_torrentzip_format() {
        use crate::plan::SourceRef;
        use sha1::Digest;
        use std::fs::File;

        let temp = TempDir::new().unwrap();

        // Create source files (added in reverse alphabetical order)
        let src1 = temp.path().join("z_last.rom");
        let src2 = temp.path().join("a_first.rom");
        fs::write(&src1, b"z data").unwrap();
        fs::write(&src2, b"a data").unwrap();

        let dest_path = temp.path().join("game.zip");

        // Compute actual SHA1 values
        let sha1_z = crate::util::hex_upper(sha1::Sha1::digest(b"z data"));
        let sha1_a = crate::util::hex_upper(sha1::Sha1::digest(b"a data"));

        let sources = vec![
            SourceRef {
                path: src1.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: sha1_z,
            },
            SourceRef {
                path: src2.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: sha1_a,
            },
        ];

        execute_repack(&sources, dest_path.to_str().unwrap(), "torrentzip").unwrap();

        // Verify the ZIP was created with sorted entries
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 2);

        // Entries should be sorted alphabetically (a_first before z_last)
        let first_name = archive.by_index(0).unwrap().name().to_string();
        let second_name = archive.by_index(1).unwrap().name().to_string();
        assert_eq!(first_name, "a_first.rom");
        assert_eq!(second_name, "z_last.rom");

        // Verify timestamp is TorrentZIP standard
        let file = File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let entry = archive.by_index(0).unwrap();
        let datetime = entry.last_modified().expect("entry has a last-modified time");
        assert_eq!(datetime.year(), 1996);
        assert_eq!(datetime.month(), 12);
        assert_eq!(datetime.day(), 24);
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

    #[test]
    fn test_get_available_space_exists() {
        // Test with a path that definitely exists
        let result = get_available_space("/tmp");
        assert!(result.is_ok());
        // Should return something > 0
        assert!(result.unwrap() > 0);
    }

    #[test]
    fn test_get_available_space_nonexistent_finds_parent() {
        // Test with a path that doesn't exist but has existing parent
        let result = get_available_space("/tmp/nonexistent_dir_12345/nested");
        assert!(result.is_ok());
        // Should return something > 0 (falls back to /tmp)
        assert!(result.unwrap() > 0);
    }

    #[test]
    fn test_check_disk_space_empty_plan() {
        let plan = Plan::new("test".to_string());
        // Empty plan should always pass
        assert!(check_disk_space(&plan).is_ok());
    }

    #[test]
    fn test_execute_rollback_move_success() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("current.rom");
        let dest_path = temp.path().join("original/file.rom");

        // Create source file (current location after apply)
        fs::write(&src_path, b"hello").unwrap();

        // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

        // Execute rollback move
        execute_rollback_move(
            src_path.to_str().unwrap(),
            dest_path.to_str().unwrap(),
            expected_sha1,
        )
        .unwrap();

        // Verify file moved to destination
        assert!(!src_path.exists());
        assert!(dest_path.exists());
        assert_eq!(fs::read(&dest_path).unwrap(), b"hello");
    }

    #[test]
    fn test_execute_rollback_move_hash_mismatch() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("current.rom");
        let dest_path = temp.path().join("original/file.rom");

        // Create source file
        fs::write(&src_path, b"hello").unwrap();

        // Wrong SHA1
        let wrong_sha1 = "0000000000000000000000000000000000000000";

        // Execute rollback move - should fail
        let result = execute_rollback_move(
            src_path.to_str().unwrap(),
            dest_path.to_str().unwrap(),
            wrong_sha1,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("hash mismatch"));

        // Source file should still exist (not moved)
        assert!(src_path.exists());
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_execute_rollback_move_source_not_found() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("nonexistent.rom");
        let dest_path = temp.path().join("dest.rom");

        let result = execute_rollback_move(
            src_path.to_str().unwrap(),
            dest_path.to_str().unwrap(),
            "somehash",
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Source file not found"));
    }

    #[test]
    fn test_check_disk_space_small_operations() {
        use crate::plan::{Operation, SourceRef};

        let mut plan = Plan::new("test".to_string());

        // Add a small copy operation to /tmp (which should have space)
        plan.operations.push(Operation {
            id: 0,
            status: OperationStatus::Pending,
            kind: OperationKind::Copy {
                source: SourceRef {
                    path: "/source/file.rom".to_string(),
                    archive_path: None,
                    sha1: "ABC123".to_string(),
                },
                dest: "/tmp/test_dest/file.rom".to_string(),
                size: 1024, // 1 KB
            },
        });

        // Small operation should pass
        assert!(check_disk_space(&plan).is_ok());
    }

    #[test]
    fn test_execute_move_loose_file() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("dest/moved.rom");

        // Create source file
        fs::write(&src_path, b"test rom content").unwrap();

        // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
        let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

        // Execute move
        execute_move(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            expected_sha1,
        )
        .unwrap();

        // Source should be deleted, destination should exist
        assert!(!src_path.exists());
        assert!(dest_path.exists());
        assert_eq!(fs::read(&dest_path).unwrap(), b"test rom content");
    }

    #[test]
    fn test_execute_move_from_archive_keeps_source() {
        let temp = TempDir::new().unwrap();

        // Create a ZIP archive with a file
        let zip_path = temp.path().join("source.zip");
        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("test.rom", options).unwrap();
            std::io::Write::write_all(&mut zip, b"hello").unwrap();
            zip.finish().unwrap();
        }

        let dest_path = temp.path().join("dest/extracted.rom");

        // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

        // Execute move from archive
        execute_move(
            zip_path.to_str().unwrap(),
            Some("test.rom"),
            dest_path.to_str().unwrap(),
            expected_sha1,
        )
        .unwrap();

        // Archive should still exist (we can't delete from inside it)
        assert!(zip_path.exists());
        // Destination should exist with extracted content
        assert!(dest_path.exists());
        assert_eq!(fs::read(&dest_path).unwrap(), b"hello");
    }

    #[test]
    fn test_execute_move_verification_fails() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("dest/moved.rom");

        // Create source file
        fs::write(&src_path, b"test rom content").unwrap();

        // Wrong SHA1
        let wrong_sha1 = "0000000000000000000000000000000000000000";

        // Execute move - should fail
        let result = execute_move(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            wrong_sha1,
        );

        assert!(result.is_err());

        // Source should still exist (copy failed before delete)
        assert!(src_path.exists());
        // Destination should not exist
        assert!(!dest_path.exists());
    }
}
