use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::fs_ops::validate_output_path;
use crate::plan::SourceRef;

/// Execute a repack operation - combine multiple source files into a single archive
///
/// Each source file is verified against its expected SHA1 before being added.
/// Supports "zip" and "torrentzip" formats.
///
/// In move mode (`move_sources`), the loose source files are deleted once the
/// archive is built and verified — a true in-place tidy. Only loose sources are
/// removed: an archive *member* source is left alone, since deleting its file
/// would destroy a container that may hold other games. The returned list pairs
/// each deleted file's canonical entry name with its original path, so the
/// caller can log a reverse that extracts it back out of the archive.
pub fn execute_repack(
    sources: &[SourceRef],
    dest_path: &str,
    format: &str,
    move_sources: bool,
) -> Result<Vec<(String, String)>> {
    let dest = Path::new(dest_path);
    validate_output_path(dest)?;

    // Create destination directory if needed
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create destination directory")?;
    }

    // An archive cannot hold two entries with the same name. The matched source
    // set can contain the same entry more than once — an entry matched via
    // several locations, or scattered across overlapping containers — which would
    // otherwise abort the build with a "Duplicate filename" error. Collapse by
    // entry name, keeping the first: a repeated name is the same matched content
    // (same SHA1), so this is lossless.
    let mut seen = std::collections::HashSet::new();
    let sources: Vec<SourceRef> = sources
        .iter()
        .filter(|s| seen.insert(get_entry_name(s).to_string()))
        .cloned()
        .collect();
    let sources = sources.as_slice();

    match format {
        "zip" => execute_repack_zip(sources, dest),
        "torrentzip" => execute_repack_torrentzip(sources, dest),
        "7z" => execute_repack_7z(sources, dest),
        _ => anyhow::bail!(
            "Unsupported repack format: {} (use 'zip', 'torrentzip', or '7z')",
            format
        ),
    }?;

    // The archive is built and every entry verified against its SHA1. Only now,
    // in move mode, consume the loose sources.
    let mut consumed = Vec::new();
    if move_sources {
        for source in sources {
            if source.archive_path.is_some() {
                continue; // never delete a shared container
            }
            let entry_name = get_entry_name(source).to_string();
            match fs::remove_file(&source.path) {
                Ok(()) => consumed.push((entry_name, source.path.clone())),
                // Already gone (e.g. a resumed run): nothing to restore for it.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to delete source after repack: {}", source.path)
                    });
                }
            }
        }
    }
    Ok(consumed)
}

/// A repack staged for concurrent execution: the operation's inputs cloned out
/// of the plan, so a worker thread owns everything it touches.
#[derive(Debug, Clone)]
pub struct RepackJob {
    /// Index of the operation in the plan's operation list, so the caller can
    /// update the right entry when the outcome arrives out of order.
    pub plan_index: usize,
    /// The plan operation's id, for the rollback journal.
    pub operation_id: u64,
    pub sources: Vec<SourceRef>,
    pub dest: String,
    pub format: String,
    pub move_sources: bool,
    /// The repacked archive's size in bytes, for progress reporting.
    pub size: u64,
}

/// The result of one concurrent repack, delivered to the caller's completion
/// callback. `consumed` carries `execute_repack`'s move-mode deletions.
pub struct RepackOutcome {
    pub job: RepackJob,
    pub result: Result<Vec<(String, String)>>,
}

/// What a repack worker reports — mirrors [`super::PlacementEvent`]. `Started`
/// fires when a worker picks a job (carrying its `slot`), `Finished` when it
/// completes, so a caller can show repacks in the same per-worker slot lanes as
/// placements.
pub enum RepackEvent {
    Started { slot: usize, plan_index: usize },
    Finished { slot: usize, outcome: RepackOutcome },
}

/// Execute a batch of repacks concurrently on a bounded pool of worker threads.
///
/// A repack is latency-bound over a network mount (read entries + recompress +
/// write + verify, each a round trip), so running ~8–16 in flight overlaps the
/// waits. Workers perform **file operations only** — each job runs the same
/// audited `execute_repack` as the serial path, including per-entry SHA-1
/// verification and move-mode delete-after-verify. Everything stateful stays
/// with the caller: `on_event` is invoked on the calling thread, one event at a
/// time, so the rollback journal, the plan status, and the (non-`Sync`)
/// catalogue connection are mutated serially. Each job reports a
/// [`RepackEvent::Started`] (with the worker's `slot`) and a
/// [`RepackEvent::Finished`].
///
/// Safe to run jobs concurrently because the planner guarantees disjointness:
/// each game repacks to its own destination archive, and a loose source shared
/// by several games is copied to each, never consumed (so no job deletes a file
/// another job reads).
pub fn execute_repacks_concurrent(
    jobs: Vec<RepackJob>,
    workers: usize,
    mut on_event: impl FnMut(RepackEvent),
) {
    if jobs.is_empty() {
        return;
    }
    let workers = workers.clamp(1, jobs.len());
    let queue = std::sync::Mutex::new(jobs.into_iter());
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::scope(|s| {
        // Each worker owns a stable `slot`, the lane a live display tracks it by.
        for slot in 0..workers {
            let tx = tx.clone();
            let queue = &queue;
            s.spawn(move || {
                loop {
                    // A poisoned lock means another worker panicked between
                    // `lock` and `next`; the iterator itself is still valid, so
                    // keep draining rather than abandoning the batch.
                    let job = queue.lock().unwrap_or_else(|p| p.into_inner()).next();
                    let Some(job) = job else { break };
                    if tx
                        .send(RepackEvent::Started {
                            slot,
                            plan_index: job.plan_index,
                        })
                        .is_err()
                    {
                        break; // receiver gone; nothing left to report to
                    }
                    let result =
                        execute_repack(&job.sources, &job.dest, &job.format, job.move_sources);
                    if tx
                        .send(RepackEvent::Finished {
                            slot,
                            outcome: RepackOutcome { job, result },
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(tx); // workers hold the remaining senders; rx ends when they finish

        for event in rx {
            on_event(event);
        }
    });
}

/// The raw bytes of a source — read from disk for a loose file, or extracted
/// from its inner archive entry.
fn source_bytes(source: &SourceRef) -> Result<Vec<u8>> {
    match &source.archive_path {
        Some(entry) => crate::archive::extract_archive_entry(Path::new(&source.path), entry),
        None => fs::read(&source.path)
            .with_context(|| format!("Failed to read source: {}", source.path)),
    }
}

/// Repack into a 7z archive (native, via sevenz-rust2), with canonical entry
/// names. Each entry's content is verified against its expected SHA1 before the
/// archive is finalised; a mismatch removes the partial archive and fails.
fn execute_repack_7z(sources: &[SourceRef], dest: &Path) -> Result<()> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    use sha1::Digest as Sha1Digest;

    let mut writer = ArchiveWriter::create(dest).context("Failed to create 7z archive")?;
    let mut verification_errors = Vec::new();

    for source in sources {
        let entry_name = get_entry_name(source);
        let data = source_bytes(source)?;

        let mut hasher = sha1::Sha1::new();
        Sha1Digest::update(&mut hasher, &data);
        let actual_sha1 = crate::util::hex_upper(Sha1Digest::finalize(hasher));
        if !actual_sha1.eq_ignore_ascii_case(&source.sha1) {
            verification_errors.push(format!(
                "{}: expected {}, got {}",
                entry_name, source.sha1, actual_sha1
            ));
            continue;
        }

        writer
            .push_archive_entry(
                ArchiveEntry::new_file(entry_name),
                Some(std::io::Cursor::new(data)),
            )
            .with_context(|| format!("Failed to add 7z entry: {}", entry_name))?;
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

    writer.finish().context("Failed to finalise 7z archive")?;
    Ok(())
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

/// Get the entry name for a source file.
///
/// Prefers an explicit `entry_name` (the DAT-canonical ROM name set by the
/// planner), so a repacked archive uses canonical names rather than whatever the
/// source file happened to be called. Falls back to the source's own name.
fn get_entry_name(source: &SourceRef) -> &str {
    if let Some(name) = source.entry_name.as_deref() {
        return name;
    }
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
pub fn extract_from_archive(archive_path: &str, entry_path: &str, dest_path: &str) -> Result<()> {
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

    // Avoid `by_name`: it misses CP437-encoded (non-UTF8-flagged) names whose
    // internal map key disagrees with `ZipFile::name()`. See
    // `crate::archive::resolve_zip_entry_index`.
    let idx = crate::archive::resolve_zip_entry_index(&mut archive, entry_path)
        .with_context(|| format!("Entry not found in archive: {}", entry_path))?;
    let mut entry = archive
        .by_index(idx)
        .with_context(|| format!("Failed to read entry: {}", entry_path))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_entry_name_prefers_canonical_entry_name() {
        // An explicit entry_name (the DAT rom name) wins over the source's own
        // file name, so repacked archives carry canonical names.
        let source = SourceRef {
            path: "/sources/whatever-it-was-called.bin".to_string(),
            archive_path: None,
            sha1: "ABC123".to_string(),
            entry_name: Some("Canonical Name.rom".to_string()),
        };
        assert_eq!(get_entry_name(&source), "Canonical Name.rom");
    }

    #[test]
    fn get_entry_name_falls_back_to_source_file_name() {
        let source = SourceRef {
            path: "/sources/game.rom".to_string(),
            archive_path: None,
            sha1: "ABC123".to_string(),
            entry_name: None,
        };
        assert_eq!(get_entry_name(&source), "game.rom");
    }
}
