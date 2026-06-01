//! Torrent file creation and verification commands

use anyhow::{Context, Result};
use lava_torrent::torrent::v1::{Torrent, TorrentBuilder};
use std::path::{Path, PathBuf};

use crate::TorrentCommands;

/// Run the torrent command
pub fn run(cmd: TorrentCommands) -> Result<()> {
    match cmd {
        TorrentCommands::Create {
            path,
            output,
            piece_size,
            tracker,
            comment,
            private,
        } => create_torrent(&path, output, piece_size, tracker, comment, private),
        TorrentCommands::Verify { torrent, path } => verify_torrent(&torrent, path),
    }
}

/// Calculate optimal piece size based on total content size
///
/// Aims for roughly 1000-2000 pieces for efficient torrent operation.
/// Minimum: 16 KiB, Maximum: 16 MiB
fn calculate_piece_size(total_size: u64) -> u64 {
    const MIN_PIECE_SIZE: u64 = 16 * 1024;        // 16 KiB
    const MAX_PIECE_SIZE: u64 = 16 * 1024 * 1024; // 16 MiB
    const TARGET_PIECES: u64 = 1500;

    let ideal_size = total_size / TARGET_PIECES;

    // Round up to nearest power of 2
    let mut piece_size = MIN_PIECE_SIZE;
    while piece_size < ideal_size && piece_size < MAX_PIECE_SIZE {
        piece_size *= 2;
    }

    piece_size.clamp(MIN_PIECE_SIZE, MAX_PIECE_SIZE)
}

/// Calculate total size of directory contents
fn calculate_directory_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;

    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            total += entry.metadata()?.len();
        }
    }

    Ok(total)
}

/// Create a torrent file from a directory
fn create_torrent(
    path: &Path,
    output: Option<PathBuf>,
    piece_size: Option<u64>,
    trackers: Vec<String>,
    comment: Option<String>,
    private: bool,
) -> Result<()> {
    // Validate input path
    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    let canonical_path = path.canonicalize()
        .with_context(|| format!("Failed to resolve path: {}", path.display()))?;

    // Determine piece size
    let piece_size = if let Some(size) = piece_size {
        // Validate user-provided piece size is power of 2
        if !size.is_power_of_two() || size < 16 * 1024 {
            anyhow::bail!(
                "Piece size must be a power of 2 and at least 16384 bytes (16 KiB)"
            );
        }
        size
    } else {
        let total_size = calculate_directory_size(&canonical_path)?;
        let auto_size = calculate_piece_size(total_size);
        println!(
            "Auto-calculated piece size: {} (for {} total)",
            crate::util::format_bytes(auto_size),
            crate::util::format_bytes(total_size)
        );
        auto_size
    };

    // Determine output path
    let output_path = output.unwrap_or_else(|| {
        let name = canonical_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "torrent".to_string());
        PathBuf::from(format!("{}.torrent", name))
    });

    println!("Creating torrent from: {}", canonical_path.display());
    println!("Output: {}", output_path.display());
    println!("Piece size: {}", crate::util::format_bytes(piece_size));

    // Build torrent
    let mut builder = TorrentBuilder::new(&canonical_path, piece_size as i64);

    // Add trackers if provided
    if !trackers.is_empty() {
        // First tracker is the announce URL
        builder = builder.set_announce(Some(trackers[0].clone()));

        // Additional trackers go in announce_list
        if trackers.len() > 1 {
            let announce_list: Vec<Vec<String>> = trackers
                .iter()
                .map(|t| vec![t.clone()])
                .collect();
            builder = builder.set_announce_list(announce_list);
        }
    }

    // Add comment using extra field
    if let Some(comment) = comment {
        builder = builder.add_extra_field(
            "comment".to_string(),
            lava_torrent::bencode::BencodeElem::String(comment),
        );
    }

    // Set private flag
    if private {
        builder = builder.set_privacy(true);
        println!("Torrent marked as private (DHT/PEX disabled)");
    }

    // Add creation info using extra field
    builder = builder.add_extra_field(
        "created by".to_string(),
        lava_torrent::bencode::BencodeElem::String(format!("cat198x/{}", env!("CARGO_PKG_VERSION"))),
    );

    println!();
    println!("Hashing files (this may take a while for large collections)...");

    let torrent = builder
        .build()
        .context("Failed to build torrent")?;

    // Extract info before writing (write_into_file consumes the torrent)
    let file_count = count_torrent_files(&torrent);
    let total_size = torrent.length;
    let piece_count = torrent.pieces.len();
    let info_hash = torrent.info_hash();

    // Write torrent file
    torrent
        .write_into_file(&output_path)
        .with_context(|| format!("Failed to write torrent file: {}", output_path.display()))?;

    println!();
    println!("Torrent created successfully!");
    println!("  Files: {}", file_count);
    println!("  Total size: {}", crate::util::format_bytes(total_size as u64));
    println!("  Pieces: {}", piece_count);
    println!("  Info hash: {}", info_hash);

    if trackers.is_empty() {
        println!();
        println!("Note: No tracker specified. Add trackers with -t/--tracker flag,");
        println!("or use DHT/magnet links for trackerless operation.");
    }

    Ok(())
}

/// Count files in a torrent
fn count_torrent_files(torrent: &Torrent) -> usize {
    match &torrent.files {
        Some(files) => files.len(),
        None => 1, // Single file torrent
    }
}

/// Verify files against a torrent
fn verify_torrent(torrent_path: &Path, base_path: Option<PathBuf>) -> Result<()> {
    // Load torrent
    if !torrent_path.exists() {
        anyhow::bail!("Torrent file does not exist: {}", torrent_path.display());
    }

    let torrent = Torrent::read_from_file(torrent_path)
        .with_context(|| format!("Failed to read torrent: {}", torrent_path.display()))?;

    // Determine base path for verification
    let base_path = base_path.unwrap_or_else(|| PathBuf::from("."));
    let base_path = base_path.canonicalize()
        .with_context(|| format!("Failed to resolve path: {}", base_path.display()))?;

    println!("Verifying torrent: {}", torrent_path.display());
    println!("Against directory: {}", base_path.display());
    println!("Info hash: {}", torrent.info_hash());
    println!();

    let file_count = count_torrent_files(&torrent);
    let total_size = torrent.length as u64;

    println!("Torrent contains {} file(s), {} total", file_count, crate::util::format_bytes(total_size));
    println!();

    // Check for files
    let mut missing = Vec::new();
    let mut found = Vec::new();
    let mut wrong_size = Vec::new();

    match &torrent.files {
        Some(files) => {
            // Multi-file torrent
            let torrent_name = &torrent.name;
            for file in files {
                let file_path = base_path.join(torrent_name).join(file.path.as_path());
                check_file(&file_path, file.length as u64, &mut found, &mut missing, &mut wrong_size);
            }
        }
        None => {
            // Single file torrent
            let file_path = base_path.join(&torrent.name);
            check_file(&file_path, total_size, &mut found, &mut missing, &mut wrong_size);
        }
    }

    // Print results
    if !missing.is_empty() {
        println!("Missing files ({}):", missing.len());
        for path in &missing {
            println!("  {}", path.display());
        }
        println!();
    }

    if !wrong_size.is_empty() {
        println!("Wrong size ({}):", wrong_size.len());
        for (path, expected, actual) in &wrong_size {
            println!(
                "  {} (expected {}, got {})",
                path.display(),
                crate::util::format_bytes(*expected),
                crate::util::format_bytes(*actual)
            );
        }
        println!();
    }

    let total = file_count;
    let ok = found.len();
    let percentage = (ok * 100).checked_div(total).unwrap_or(100);

    println!("Verification complete:");
    println!("  Found: {}/{} ({}%)", ok, total, percentage);
    println!("  Missing: {}", missing.len());
    println!("  Wrong size: {}", wrong_size.len());

    if missing.is_empty() && wrong_size.is_empty() {
        println!();
        println!("All files present and correct sizes!");
        println!();
        println!("Note: This check only verifies file presence and sizes.");
        println!("Full piece hash verification would require reading all file contents.");
        Ok(())
    } else {
        anyhow::bail!("Verification failed: {} missing, {} wrong size", missing.len(), wrong_size.len());
    }
}

/// Check a single file for existence and size
fn check_file(
    path: &Path,
    expected_size: u64,
    found: &mut Vec<PathBuf>,
    missing: &mut Vec<PathBuf>,
    wrong_size: &mut Vec<(PathBuf, u64, u64)>,
) {
    if !path.exists() {
        missing.push(path.to_path_buf());
        return;
    }

    match path.metadata() {
        Ok(meta) => {
            let actual_size = meta.len();
            if actual_size == expected_size {
                found.push(path.to_path_buf());
            } else {
                wrong_size.push((path.to_path_buf(), expected_size, actual_size));
            }
        }
        Err(_) => {
            missing.push(path.to_path_buf());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_piece_size_small() {
        // Small file (1 MB) should get minimum piece size
        let size = calculate_piece_size(1024 * 1024);
        assert_eq!(size, 16 * 1024); // 16 KiB minimum
    }

    #[test]
    fn test_calculate_piece_size_medium() {
        // 100 MB / 1500 target pieces ≈ 68KB → rounds up to 128 KiB (next power of 2)
        let size = calculate_piece_size(100 * 1024 * 1024);
        assert_eq!(size, 128 * 1024); // 128 KiB
    }

    #[test]
    fn test_calculate_piece_size_large() {
        // 10 GB should get ~8 MiB pieces
        let size = calculate_piece_size(10 * 1024 * 1024 * 1024);
        assert_eq!(size, 8 * 1024 * 1024); // 8 MiB
    }

    #[test]
    fn test_calculate_piece_size_very_large() {
        // 100 GB should cap at maximum
        let size = calculate_piece_size(100 * 1024 * 1024 * 1024);
        assert_eq!(size, 16 * 1024 * 1024); // 16 MiB maximum
    }

    #[test]
    fn test_piece_size_is_power_of_two() {
        for size_mb in [1, 10, 100, 1000, 10000] {
            let piece_size = calculate_piece_size(size_mb * 1024 * 1024);
            assert!(piece_size.is_power_of_two(), "Piece size {} should be power of 2", piece_size);
        }
    }
}
