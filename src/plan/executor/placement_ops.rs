use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::archive_ops::extract_from_archive;
use super::fs_ops::{hash_file, validate_output_path, verify_written_sha1};
use crate::plan::{CopyPlacement, SourceRef};

/// Execute a copy operation from source to destination with verification
///
/// If dest_path ends with .zip, the file will be written into a ZIP archive.
/// The entry name inside the ZIP is derived from the dest_path filename without .zip extension.
pub fn execute_copy(
    source_path: &str,
    archive_path: Option<&str>,
    dest_path: &str,
    expected_sha1: &str,
    placement: &CopyPlacement,
) -> Result<()> {
    let dest = Path::new(dest_path);
    validate_output_path(dest)?;

    if let CopyPlacement::ZipEntry { entry_name } = placement {
        return execute_copy_to_zip(
            source_path,
            archive_path,
            dest_path,
            entry_name,
            expected_sha1,
        );
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
    placement: &CopyPlacement,
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
    if archive_path.is_none() && matches!(placement, CopyPlacement::LooseFile) {
        let source = Path::new(source_path);
        let dest = Path::new(dest_path);
        validate_output_path(dest)?;

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).context("Failed to create destination directory")?;
        }
        if fs::rename(source, dest).is_ok() {
            return Ok(());
        }
    }

    // Copy path: cross-device move, archive source, or archive destination.
    execute_copy(
        source_path,
        archive_path,
        dest_path,
        expected_sha1,
        placement,
    )?;

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
    validate_output_path(dest)?;

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

/// Execute a copy operation where the destination is a ZIP archive
///
/// The entry name inside the ZIP is derived from the source file name.
fn execute_copy_to_zip(
    source_path: &str,
    archive_path: Option<&str>,
    dest_path: &str,
    entry_name: &str,
    expected_sha1: &str,
) -> Result<()> {
    use crate::archive::{ZipWriter, ZipWriterOptions};

    let dest = Path::new(dest_path);
    validate_output_path(dest)?;

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

/// One placement operation — a copy, move, or relocate — dispatched to a worker.
#[derive(Debug, Clone)]
pub struct PlacementJob {
    /// Index of the operation in the plan, so the caller updates the right entry
    /// when outcomes arrive out of order.
    pub plan_index: usize,
    /// The plan operation's id, for the rollback journal.
    pub operation_id: u64,
    pub kind: PlacementKind,
}

/// The three placement operations a worker can run. Each carries exactly what its
/// audited executor needs — no catalogue, so it is safe off the calling thread.
#[derive(Debug, Clone)]
pub enum PlacementKind {
    Copy {
        source: SourceRef,
        dest: String,
        placement: CopyPlacement,
    },
    Move {
        source: SourceRef,
        dest: String,
        placement: CopyPlacement,
    },
    Relocate {
        source: String,
        dest: String,
    },
}

/// The result of one concurrent placement, delivered to the completion callback.
pub struct PlacementOutcome {
    pub job: PlacementJob,
    pub result: Result<()>,
}

/// What a worker reports as it runs the batch. `Started` fires when a worker
/// picks a job (so a caller can show which of its `slot`s is now busy with which
/// operation); `Finished` fires when that job completes. Both carry the worker's
/// `slot` (`0..workers`), the stable lane that lets a live display track one
/// worker across the jobs it runs.
pub enum PlacementEvent {
    Started {
        slot: usize,
        plan_index: usize,
    },
    Finished {
        slot: usize,
        outcome: PlacementOutcome,
    },
}

/// Execute a batch of placements (copy / move / relocate) concurrently on a
/// bounded pool of worker threads — the same contract as
/// [`super::execute_repacks_concurrent`], for the same reason: each placement is
/// latency-bound over a network mount (read/extract + write + verify, each a
/// round trip), so overlapping several hides the waits.
///
/// Workers perform **file operations only**, each running the same audited
/// `execute_copy`/`execute_move`/`execute_relocate` as the serial path —
/// including SHA-1 verification and (for a move) delete-after-verify. Everything
/// stateful stays with the caller: `on_event` runs on the calling thread, one
/// event at a time, so the rollback journal, the plan status, and the non-`Sync`
/// catalogue connection are all mutated serially. Each job reports a
/// [`PlacementEvent::Started`] (carrying the worker's `slot`) when a worker picks
/// it up and a [`PlacementEvent::Finished`] when it completes, so a caller can
/// drive a live per-worker display alongside the completion bookkeeping.
///
/// Safe to run concurrently because the planner guarantees the batch is disjoint:
/// distinct content has distinct destinations, and content shared between entries
/// is copied (never moved), so no job reads a file another job is deleting.
pub fn execute_placements_concurrent(
    jobs: Vec<PlacementJob>,
    workers: usize,
    mut on_event: impl FnMut(PlacementEvent),
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
                    let job = queue.lock().unwrap_or_else(|p| p.into_inner()).next();
                    let Some(job) = job else { break };
                    if tx
                        .send(PlacementEvent::Started {
                            slot,
                            plan_index: job.plan_index,
                        })
                        .is_err()
                    {
                        break; // receiver gone; nothing left to report to
                    }
                    let result = match &job.kind {
                        PlacementKind::Copy {
                            source,
                            dest,
                            placement,
                        } => execute_copy(
                            &source.path,
                            source.archive_path.as_deref(),
                            dest,
                            &source.sha1,
                            placement,
                        ),
                        PlacementKind::Move {
                            source,
                            dest,
                            placement,
                        } => execute_move(
                            &source.path,
                            source.archive_path.as_deref(),
                            dest,
                            &source.sha1,
                            placement,
                        ),
                        PlacementKind::Relocate { source, dest } => execute_relocate(source, dest),
                    };
                    if tx
                        .send(PlacementEvent::Finished {
                            slot,
                            outcome: PlacementOutcome { job, result },
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
