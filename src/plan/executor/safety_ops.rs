use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::fs_ops::{validate_output_path, verify_written_sha1};

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
            if Path::new(&format!("{}/{}", root, loc.path)).exists() {
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
    validate_output_path(dest_path)?;

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
            fs::File::open(dest_path)
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
