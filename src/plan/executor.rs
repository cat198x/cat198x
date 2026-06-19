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
use crate::scanner::chd;
use crate::util::{format_bytes, verify_sha1};

/// Confirm every content held at `abs_path` also exists in another physical
/// location on disk, so removing the file cannot destroy the only copy.
///
/// A delete is decided from the catalogue, but the catalogue may have drifted
/// since — a copy recorded then may have moved or gone. Re-checking on disk at
/// delete time means a stale record can't turn a delete into data loss. Returns
/// false — refuse the delete — if the path's source can't be resolved, its
/// contents aren't catalogued, or any content has no surviving on-disk copy
/// outside this path.
///
/// The surviving copy must satisfy the source's disposition
/// (`decisions/source-disposition.md`, the delete rule): a `consume` source may
/// be emptied, so a copy in **any** other location counts; a `preserve` source
/// must never lose content its tree alone holds, so only a copy **in the same
/// tree** (same source) counts — a copy in another tree does not authorise the
/// delete. An unresolved source is treated as `preserve`, the strict default.
///
/// This is the shared verify-before-delete net: `apply`'s delete operations and
/// `clean-superseded` both gate on it so the safety check can't drift between
/// them.
pub fn delete_has_surviving_copy(
    conn: &rusqlite::Connection,
    sources: &[crate::db::files::Source],
    abs_path: &str,
) -> Result<bool> {
    use crate::db::files::{self, Disposition};

    let Some((source_id, rel)) = files::resolve_in_sources(sources, abs_path) else {
        return Ok(false);
    };
    // A preserve tree may only be deduped against itself; a copy elsewhere must
    // not authorise removing this tree's content. An unknown source stays strict.
    let preserve = sources
        .iter()
        .find(|s| s.id == source_id)
        .map(|s| matches!(s.disposition, Disposition::Preserve))
        .unwrap_or(true);
    let sha1s = files::contents_at_location(conn, source_id, &rel)?;
    if sha1s.is_empty() {
        return Ok(false);
    }
    for sha1 in &sha1s {
        let mut survives = false;
        for loc in files::get_file_locations(conn, sha1)? {
            // The copy we're about to delete doesn't count as its own backup.
            if loc.source_id == source_id && loc.path == rel {
                continue;
            }
            // For a preserve-tree file, only a surviving copy within the same
            // tree counts — a copy in another tree must not justify the delete.
            if preserve && loc.source_id != source_id {
                continue;
            }
            let Some(root) = sources
                .iter()
                .find(|s| s.id == loc.source_id)
                .map(|s| s.path.trim_end_matches('/').to_string())
            else {
                continue;
            };
            if std::path::Path::new(&format!("{}/{}", root, loc.path)).exists() {
                survives = true;
                break;
            }
        }
        if !survives {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Verify a written file against its catalogued SHA1.
///
/// A CHD is catalogued by its *internal* (logical-data) SHA1, read from the
/// header — not by the hash of the `.chd` file's bytes, which changes with the
/// compression used. Hashing the whole file would never match the catalogue, so
/// a `.chd` is verified by re-reading its header SHA1; every other file is a
/// full-file hash. A byte-for-byte copy or rename preserves the header, so the
/// internal SHA1 is exactly as strong a check here as a content hash is for a
/// loose ROM.
fn verify_written_sha1(path: &Path, expected: &str) -> Result<bool> {
    if chd::is_chd_path(path) {
        Ok(chd::read_chd_sha1(path)?.eq_ignore_ascii_case(expected))
    } else {
        verify_sha1(path, expected)
    }
}

/// Move a file into the content-addressed quarantine store and record it.
///
/// The quarantine filename is `<full-sha1>_<original-name>` — the full hash (not
/// a prefix) means two distinct files can never collide onto one path, and an
/// existing target is refused rather than overwritten. The move is rename-first,
/// copy+delete on a cross-device failure, and the catalogue entry is added on the
/// caller's connection. Returns the quarantine path so the caller can journal the
/// move and reverse it (restore to the original) on rollback.
///
/// This is the file-operation half of quarantining; resolving *where* the store
/// lives (config vs default) stays with the caller, which passes `quarantine_dir`.
pub fn execute_quarantine(
    conn: &rusqlite::Connection,
    file_path: &str,
    sha1: &str,
    size: i64,
    reason: crate::db::quarantine::QuarantineReason,
    collection_name: Option<&str>,
    quarantine_dir: &Path,
) -> Result<String> {
    fs::create_dir_all(quarantine_dir).context("Failed to create quarantine directory")?;

    let original_filename = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let quarantine_filename = format!("{sha1}_{original_filename}");
    let quarantine_path = quarantine_dir.join(&quarantine_filename);

    // Never overwrite an existing quarantine file — a collision under the full
    // SHA1 means identical content under the same name, so refuse rather than
    // clobber whatever is already there.
    if quarantine_path.exists() {
        anyhow::bail!(
            "Quarantine target already exists, refusing to overwrite: {}",
            quarantine_path.display()
        );
    }

    let source = Path::new(file_path);
    if source.exists() {
        fs::rename(source, &quarantine_path).or_else(|_| {
            // Cross-device rename fails; fall back to copy + delete.
            fs::copy(source, &quarantine_path)?;
            fs::remove_file(source)?;
            Ok::<_, anyhow::Error>(())
        })?;
    } else {
        anyhow::bail!("File not found: {}", file_path);
    }

    crate::db::quarantine::add_entry(
        conn,
        sha1,
        file_path,
        &quarantine_filename,
        size,
        reason,
        collection_name,
    )?;

    Ok(quarantine_path.to_string_lossy().into_owned())
}

/// Execute a rollback move operation
pub fn execute_rollback_move(source: &str, dest: &str, expected_sha1: &str) -> Result<()> {
    let source_path = Path::new(source);
    let dest_path = Path::new(dest);

    // Verify source file has expected hash
    if source_path.exists() {
        if !verify_written_sha1(source_path, expected_sha1)? {
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
            if !verify_written_sha1(dest_path, expected_sha1)? {
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

    // Verify the written file matches expected hash (CHDs by their internal
    // header SHA1, since the file-byte hash changes with compression).
    if !verify_written_sha1(dest, expected_sha1)? {
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
    // Fast path: a loose-file move to a loose-file destination on the same
    // filesystem is an atomic rename — no bytes copied. This is the common case
    // for an in-place tidy and turns a full read+write+read of every ROM into a
    // metadata operation. A rename preserves the bytes exactly, so we trust the
    // catalogue's recorded hash rather than re-reading every file over a
    // (possibly networked) source to verify it first — the same trade-off
    // execute_relocate makes. A rename failure (almost always a cross-device
    // link error), an archive source, or an archive (.zip) destination falls
    // through to the copy path below, which *does* verify the content.
    if archive_path.is_none() && !dest_path.to_lowercase().ends_with(".zip") {
        let source = Path::new(source_path);
        let dest = Path::new(dest_path);

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).context("Failed to create destination directory")?;
        }
        if fs::rename(source, dest).is_ok() {
            return Ok(());
        }
    }

    // Copy path: cross-device move, archive source, or archive destination.
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
        fs::remove_file(source)
            .with_context(|| format!("Failed to delete source file after move: {}", source_path))?;
    }

    Ok(())
}

/// Relocate a whole file unchanged to `dest`.
///
/// A same-filesystem rename moves the bytes atomically with no copy — the common
/// case for staging a complete archive into the library on one volume. A rename
/// failure (cross-device) falls back to a copy that is then verified byte-faithful
/// to the source by re-hashing both (the file's own hash isn't catalogued), and
/// the source is removed only after the copy is flushed to disk.
pub fn execute_relocate(source_path: &str, dest_path: &str) -> Result<()> {
    let source = Path::new(source_path);
    let dest = Path::new(dest_path);

    if !source.exists() {
        anyhow::bail!("Source file not found: {}", source_path);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create destination directory")?;
    }

    if fs::rename(source, dest).is_ok() {
        return Ok(());
    }

    // Cross-device: copy, confirm byte-faithful, flush, then remove the source.
    fs::copy(source_path, dest_path).context("Failed to copy file during relocate")?;
    if hash_file(source)? != hash_file(dest)? {
        let _ = fs::remove_file(dest);
        anyhow::bail!(
            "Relocate copy is not byte-faithful to source: {}",
            source_path
        );
    }
    std::fs::File::open(dest_path)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("Failed to flush destination before delete: {}", dest_path))?;
    fs::remove_file(source)
        .with_context(|| format!("Failed to delete source after relocate: {}", source_path))?;
    Ok(())
}

/// SHA-1 of a whole file, upper-case hex.
fn hash_file(path: &Path) -> Result<String> {
    use sha1::{Digest, Sha1};
    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut hasher = Sha1::new();
    Digest::update(&mut hasher, &bytes);
    Ok(crate::util::hex_upper(Digest::finalize(hasher)))
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

/// Execute a batch of repacks concurrently on a bounded pool of worker threads.
///
/// A repack is latency-bound over a network mount (read entries + recompress +
/// write + verify, each a round trip), so running ~8–16 in flight overlaps the
/// waits. Workers perform **file operations only** — each job runs the same
/// audited `execute_repack` as the serial path, including per-entry SHA-1
/// verification and move-mode delete-after-verify. Everything stateful stays
/// with the caller: `on_complete` is invoked on the calling thread, one outcome
/// at a time, as jobs finish — so the rollback journal, the plan status, and
/// the (non-`Sync`) catalogue connection are mutated serially exactly as in
/// serial execution, just in completion order rather than plan order.
///
/// Safe to run jobs concurrently because the planner guarantees disjointness:
/// each game repacks to its own destination archive, and a loose source shared
/// by several games is copied to each, never consumed (so no job deletes a file
/// another job reads).
pub fn execute_repacks_concurrent(
    jobs: Vec<RepackJob>,
    workers: usize,
    mut on_complete: impl FnMut(RepackOutcome),
) {
    if jobs.is_empty() {
        return;
    }
    let workers = workers.clamp(1, jobs.len());
    let queue = std::sync::Mutex::new(jobs.into_iter());
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let queue = &queue;
            s.spawn(move || {
                loop {
                    // A poisoned lock means another worker panicked between
                    // `lock` and `next`; the iterator itself is still valid, so
                    // keep draining rather than abandoning the batch.
                    let job = queue.lock().unwrap_or_else(|p| p.into_inner()).next();
                    let Some(job) = job else { break };
                    let result =
                        execute_repack(&job.sources, &job.dest, &job.format, job.move_sources);
                    if tx.send(RepackOutcome { job, result }).is_err() {
                        break; // receiver gone; nothing left to report to
                    }
                }
            });
        }
        drop(tx); // workers hold the remaining senders; rx ends when they finish

        for outcome in rx {
            on_complete(outcome);
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

/// Check there's enough free space for everything the plan will write.
///
/// Counts only genuinely new bytes, grouped by destination volume. A same-volume
/// `Move` or `Relocate` is a rename — it frees as much as it consumes, so it
/// needs no space; only copies, cross-volume moves, and the transient archive a
/// repack builds count. This matters for a `--move` in-place tidy, where the
/// moves dominate the byte total yet need no space at all.
///
/// Bucketing by volume (not per destination directory) also keeps this to one
/// free-space query per volume instead of thousands of `stat`s over a network
/// mount, and the repack size comes from the plan rather than from stat-ing
/// every source file.
pub fn check_disk_space(plan: &Plan) -> Result<()> {
    let mut bytes_by_volume: HashMap<String, u64> = HashMap::new();

    for op in &plan.operations {
        if op.status != OperationStatus::Pending {
            continue;
        }

        let (dest, needed): (&str, u64) = match &op.kind {
            OperationKind::Copy { dest, size, .. } => (dest, *size),
            OperationKind::Move { source, dest, size } => (
                dest,
                if same_volume(&source.path, dest) {
                    0
                } else {
                    *size
                },
            ),
            OperationKind::Relocate { source, dest, size } => {
                (dest, if same_volume(source, dest) { 0 } else { *size })
            }
            // A repack builds a new archive at dest; while it is written its
            // sources still exist (move-mode deletion happens only after the
            // archive verifies), so the transient peak is the archive size.
            OperationKind::Repack { dest, size, .. } => (dest, *size),
            // Deletes free space.
            OperationKind::Delete { .. } => continue,
            // Quarantine writes into the data dir, a separate space concern from
            // the library volume — not checked here.
            OperationKind::Quarantine { .. } => continue,
        };

        if needed == 0 {
            continue;
        }
        *bytes_by_volume.entry(volume_root(dest)).or_insert(0) += needed;
    }

    for (volume, bytes_needed) in &bytes_by_volume {
        let available = get_available_space(volume)?;

        // Add 10% safety margin
        let bytes_with_margin = (*bytes_needed as f64 * 1.1) as u64;

        if available < bytes_with_margin {
            anyhow::bail!(
                "Insufficient space on '{}': need {} (with 10% margin), have {}",
                volume,
                format_bytes(bytes_with_margin),
                format_bytes(available)
            );
        }
    }

    Ok(())
}

/// The volume root of an absolute path — `/Volumes/<name>` for a mounted volume,
/// otherwise `/`. A string test, so it costs no `stat` over a network mount.
/// A nested mount under `/Volumes/<name>` is treated as the same volume; the
/// library is one tree per volume, so this is exact in practice and only ever
/// conservative (it never under-reserves space).
fn volume_root(path: &str) -> String {
    let mut comps = path.trim_start_matches('/').split('/');
    match (comps.next(), comps.next()) {
        (Some("Volumes"), Some(name)) if !name.is_empty() => format!("/Volumes/{name}"),
        _ => "/".to_string(),
    }
}

/// Whether two paths live on the same volume, so a move between them is a rename
/// that needs no new space.
fn same_volume(a: &str, b: &str) -> bool {
    volume_root(a) == volume_root(b)
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Verification failed")
        );

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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Verification failed")
        );

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
                entry_name: None,
            },
            SourceRef {
                path: src2.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "75BF07C00E138F33E12904F575641F0C06CBB838".to_string(), // SHA1 of "graphics data"
                entry_name: None,
            },
        ];

        execute_repack(&sources, dest_path.to_str().unwrap(), "zip", false).unwrap();

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
    fn execute_repack_dedupes_duplicate_entry_names() {
        use crate::plan::SourceRef;
        use std::io::Read;

        let temp = TempDir::new().unwrap();
        let src = temp.path().join("data.rom");
        fs::write(&src, b"cpu data").unwrap();
        let dest_path = temp.path().join("game.zip");

        // The same entry name appears twice in the source set (an entry matched
        // via two locations). A ZIP can't hold a duplicate name, so the repack
        // must collapse them rather than abort with "Duplicate filename".
        let one = SourceRef {
            path: src.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "76218C22675632AEF6A27578DD0A2C6471D995D5".to_string(), // "cpu data"
            entry_name: Some("game.rom".to_string()),
        };
        let sources = vec![one.clone(), one];

        execute_repack(&sources, dest_path.to_str().unwrap(), "zip", false).unwrap();

        let file = fs::File::open(&dest_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1, "duplicate entry name collapsed to one");
        let mut entry = archive.by_name("game.rom").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"cpu data");
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
                entry_name: None,
            },
            SourceRef {
                path: src2.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "0000000000000000000000000000000000000000".to_string(), // Wrong hash
                entry_name: None,
            },
        ];

        let result = execute_repack(&sources, dest_path.to_str().unwrap(), "zip", false);

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
    fn test_execute_repack_unsupported_format() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();
        let src = temp.path().join("file.rom");
        fs::write(&src, b"data").unwrap();

        let dest_path = temp.path().join("game.rar");

        let sources = vec![SourceRef {
            path: src.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "A17C9AAA61E80A1BF71D0D850AF4E5BAA9800BBD".to_string(), // SHA1 of "data"
            entry_name: None,
        }];

        let result = execute_repack(&sources, dest_path.to_str().unwrap(), "rar", false);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unsupported repack format")
        );
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
                entry_name: None,
            },
            SourceRef {
                path: src2.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: sha1_a,
                entry_name: None,
            },
        ];

        execute_repack(&sources, dest_path.to_str().unwrap(), "torrentzip", false).unwrap();

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
        let datetime = entry
            .last_modified()
            .expect("entry has a last-modified time");
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Source file not found")
        );
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
                    entry_name: None,
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
    fn test_execute_move_same_fs_renames_without_rehashing() {
        let temp = TempDir::new().unwrap();
        let src_path = temp.path().join("source.rom");
        let dest_path = temp.path().join("dest/moved.rom");
        fs::write(&src_path, b"test rom content").unwrap();

        // A same-filesystem loose move is a pure rename that trusts the
        // catalogue's recorded hash: it does not re-read the file to verify it
        // first. A rename preserves the bytes exactly, so even a hash that does
        // not match the content still moves it. This guards the performance fix
        // that turned re-reading every ROM over a network mount back into a
        // metadata-only rename.
        let wrong_sha1 = "0000000000000000000000000000000000000000";
        execute_move(
            src_path.to_str().unwrap(),
            None,
            dest_path.to_str().unwrap(),
            wrong_sha1,
        )
        .unwrap();

        assert!(!src_path.exists(), "source renamed away");
        assert_eq!(fs::read(&dest_path).unwrap(), b"test rom content");
    }

    #[test]
    fn execute_relocate_moves_whole_file_unchanged() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("ToSort/SET/Game.zip");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"a complete zip's bytes").unwrap();
        // Destination in a not-yet-existing nested dir (same filesystem → rename).
        let dest = temp.path().join("ROMs/SET/Sys/Game.zip");

        execute_relocate(src.to_str().unwrap(), dest.to_str().unwrap()).unwrap();

        assert!(!src.exists(), "source relocated away");
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"a complete zip's bytes");
    }

    #[test]
    fn execute_relocate_missing_source_errors() {
        let temp = TempDir::new().unwrap();
        let result = execute_relocate(
            temp.path().join("nope.zip").to_str().unwrap(),
            temp.path().join("dest.zip").to_str().unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn execute_repack_7z_writes_canonical_entries() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();
        // Source file with a non-canonical name; SHA1 of "cpu data".
        let src = temp.path().join("whatever-it-was-called.bin");
        fs::write(&src, b"cpu data").unwrap();
        let expected_sha1 = "76218C22675632AEF6A27578DD0A2C6471D995D5";

        let dest = temp.path().join("game.7z");
        let sources = vec![SourceRef {
            path: src.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: expected_sha1.to_string(),
            entry_name: Some("canonical.rom".to_string()),
        }];

        execute_repack(&sources, dest.to_str().unwrap(), "7z", false).unwrap();
        assert!(dest.exists());

        // Read back: one entry, named canonically, with the right content hash.
        let entries = crate::scanner::archive::hash_archive_entries(&dest).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "canonical.rom");
        assert!(
            entries[0]
                .hashes
                .as_ref()
                .unwrap()
                .sha1
                .eq_ignore_ascii_case(expected_sha1)
        );
    }

    #[test]
    fn execute_repacks_concurrent_runs_all_jobs() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();
        // SHA1 of "cpu data"
        let sha1 = "76218C22675632AEF6A27578DD0A2C6471D995D5";

        // More jobs than workers, so the queue actually round-robins.
        let jobs: Vec<RepackJob> = (0..10)
            .map(|i| {
                let src = temp.path().join(format!("src-{i}.rom"));
                fs::write(&src, b"cpu data").unwrap();
                RepackJob {
                    plan_index: i,
                    operation_id: i as u64 + 100,
                    sources: vec![SourceRef {
                        path: src.to_str().unwrap().to_string(),
                        archive_path: None,
                        sha1: sha1.to_string(),
                        entry_name: Some("game.rom".to_string()),
                    }],
                    dest: temp
                        .path()
                        .join(format!("game-{i}.zip"))
                        .to_str()
                        .unwrap()
                        .to_string(),
                    format: "zip".to_string(),
                    move_sources: false,
                    size: 8,
                }
            })
            .collect();
        let dests: Vec<String> = jobs.iter().map(|j| j.dest.clone()).collect();

        // The callback mutates plain locals with no synchronisation — proof it
        // runs on the calling thread, the property apply relies on to keep the
        // journal and catalogue updates serial.
        let mut seen = Vec::new();
        execute_repacks_concurrent(jobs, 4, |outcome| {
            assert!(outcome.result.is_ok(), "{:?}", outcome.result.err());
            seen.push(outcome.job.plan_index);
        });

        seen.sort_unstable();
        assert_eq!(seen, (0..10).collect::<Vec<_>>(), "every job reported once");
        for dest in dests {
            assert!(Path::new(&dest).exists(), "archive built: {dest}");
        }
    }

    #[test]
    fn execute_repacks_concurrent_reports_failures_individually() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();
        let good_src = temp.path().join("good.rom");
        let bad_src = temp.path().join("bad.rom");
        fs::write(&good_src, b"cpu data").unwrap();
        fs::write(&bad_src, b"cpu data").unwrap();

        let make_job = |idx: usize, src: &Path, sha1: &str| RepackJob {
            plan_index: idx,
            operation_id: idx as u64,
            sources: vec![SourceRef {
                path: src.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: sha1.to_string(),
                entry_name: None,
            }],
            dest: temp
                .path()
                .join(format!("out-{idx}.zip"))
                .to_str()
                .unwrap()
                .to_string(),
            format: "zip".to_string(),
            move_sources: false,
            size: 8,
        };

        let jobs = vec![
            make_job(0, &good_src, "76218C22675632AEF6A27578DD0A2C6471D995D5"),
            make_job(1, &bad_src, "0000000000000000000000000000000000000000"),
        ];
        let good_dest = jobs[0].dest.clone();
        let bad_dest = jobs[1].dest.clone();

        let mut outcomes: Vec<(usize, bool)> = Vec::new();
        execute_repacks_concurrent(jobs, 2, |o| {
            outcomes.push((o.job.plan_index, o.result.is_ok()));
        });
        outcomes.sort_unstable();

        // One job failing verification doesn't take the batch down: the good
        // job still builds, the bad one reports Err and removed its partial.
        assert_eq!(outcomes, vec![(0, true), (1, false)]);
        assert!(Path::new(&good_dest).exists());
        assert!(!Path::new(&bad_dest).exists());
    }

    #[test]
    fn execute_repack_7z_verifies_content_hash() {
        use crate::plan::SourceRef;

        let temp = TempDir::new().unwrap();
        let src = temp.path().join("a.bin");
        fs::write(&src, b"cpu data").unwrap();
        let dest = temp.path().join("bad.7z");

        // Wrong expected hash → repack fails and removes the partial archive.
        let sources = vec![SourceRef {
            path: src.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "0000000000000000000000000000000000000000".to_string(),
            entry_name: Some("x.rom".to_string()),
        }];

        assert!(execute_repack(&sources, dest.to_str().unwrap(), "7z", false).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn volume_root_and_same_volume() {
        assert_eq!(
            volume_root("/Volumes/Data/Library/ROMs/x.zip"),
            "/Volumes/Data"
        );
        assert_eq!(volume_root("/Volumes/Data"), "/Volumes/Data");
        assert_eq!(volume_root("/Users/me/roms/x.zip"), "/");
        assert_eq!(volume_root("/"), "/");
        // ToSort and Library on the same volume compare equal.
        assert!(same_volume(
            "/Volumes/Data/ToSort/MAME/g.zip",
            "/Volumes/Data/Library/ROMs/MAME/g.zip"
        ));
        // Different volumes do not.
        assert!(!same_volume("/Volumes/Data/x.zip", "/Volumes/Backup/x.zip"));
    }

    #[test]
    fn check_disk_space_ignores_same_volume_moves() {
        // A same-volume move is a rename — it needs no space, however large. Were
        // it counted, this u64::MAX move would fail the check; it must pass.
        let mut plan = Plan::new("h".to_string());
        let src = SourceRef {
            path: "/Volumes/Data/ToSort/big.bin".to_string(),
            archive_path: None,
            sha1: "a".to_string(),
            entry_name: None,
        };
        plan.add_move(src, "/Volumes/Data/Library/big.bin".to_string(), u64::MAX);
        assert!(check_disk_space(&plan).is_ok());
    }

    #[test]
    fn check_disk_space_counts_cross_volume_moves() {
        // A cross-volume move genuinely needs space at the destination, so an
        // impossible u64::MAX move must be refused.
        let mut plan = Plan::new("h".to_string());
        let src = SourceRef {
            path: "/Volumes/Data/big.bin".to_string(),
            archive_path: None,
            sha1: "a".to_string(),
            entry_name: None,
        };
        plan.add_move(src, "/Volumes/Backup/big.bin".to_string(), u64::MAX);
        assert!(check_disk_space(&plan).is_err());
    }

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
