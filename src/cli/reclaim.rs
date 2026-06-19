//! `reclaim` command — free space by deleting a source's files whose every
//! content is already held in another source.
//!
//! The motivating case: after a reorg moves a set into the library, the staging
//! input (e.g. `ToSort/…`) is left holding archives whose every ROM now lives in
//! the library too. Those husks are pure redundancy — deleting them frees the
//! space without losing a byte, because each content survives in the other source.
//!
//! Safety model (hard delete has no undo):
//! - **Dry-run by default.** A plain `reclaim` only reports; `--execute` deletes.
//! - **Cross-source only.** A file is reclaimable only when every content it holds
//!   is *also* catalogued in a *different* source — so the deleted copy is never
//!   the last one. A source's own unique content is never reclaimed.
//! - **Existence-verified delete.** Before removing a file, each of its contents'
//!   external copies is confirmed to physically exist on disk (not just in the
//!   catalogue), so a stale "held elsewhere" record can't cause data loss.
//! - **Journaled.** Each run writes an audit log of what it removed.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::db::files::{self, resolve_in_sources};

use super::{get_data_dir, open_database};

/// A file (loose file or whole archive container) that can be reclaimed.
#[derive(Debug, Clone)]
struct ReclaimTarget {
    /// Absolute path to delete.
    full_path: String,
    /// Bytes freed by deleting it.
    bytes: i64,
    /// Distinct contents (sha1) it holds — for the existence-verified delete.
    sha1s: Vec<String>,
    /// `true` for a whole archive container, `false` for a loose file.
    is_archive: bool,
}

/// Whether a `--source` selector picks this source: a numeric selector is a
/// source id (exact); anything else matches as a path substring.
fn source_matches(source: &files::Source, selector: &str) -> bool {
    match selector.parse::<i64>() {
        Ok(id) => source.id == id,
        Err(_) => source.path.contains(selector),
    }
}

/// The files in `source_id` whose every content is also held in another source.
fn compute_reclaimable(conn: &rusqlite::Connection, source_id: i64) -> Result<Vec<ReclaimTarget>> {
    let mut targets = Vec::new();

    // Loose files: reclaimable when this content is held in another source.
    let mut stmt = conn.prepare(
        "SELECT s.path || '/' || fl.path, f.size, fl.sha1
           FROM file_locations fl
           JOIN files f ON f.sha1 = fl.sha1
           JOIN sources s ON s.id = fl.source_id
          WHERE fl.source_id = ?1 AND fl.archive_path IS NULL
            AND EXISTS (SELECT 1 FROM file_locations o
                         WHERE o.sha1 = fl.sha1 AND o.source_id <> ?1)",
    )?;
    let rows = stmt.query_map([source_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for r in rows {
        let (full_path, bytes, sha1) = r?;
        targets.push(ReclaimTarget {
            full_path,
            bytes,
            sha1s: vec![sha1],
            is_archive: false,
        });
    }

    // Archive containers: reclaimable only when *every* entry is held in another
    // source (no entry is unique to this container).
    let mut stmt = conn.prepare(
        "SELECT s.path || '/' || fl.path, SUM(f.size)
           FROM file_locations fl
           JOIN files f ON f.sha1 = fl.sha1
           JOIN sources s ON s.id = fl.source_id
          WHERE fl.source_id = ?1 AND fl.archive_path IS NOT NULL
          GROUP BY fl.source_id, fl.path
         HAVING SUM(CASE WHEN EXISTS (
                  SELECT 1 FROM file_locations o
                   WHERE o.sha1 = fl.sha1 AND o.source_id <> ?1
                ) THEN 0 ELSE 1 END) = 0",
    )?;
    let container_rows = stmt
        .query_map([source_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // Collect each reclaimable container's distinct entry hashes for verification.
    for (full_path, bytes) in container_rows {
        let rel = full_path
            .strip_prefix(&prefix_for(conn, source_id)?)
            .map(|p| p.trim_start_matches('/').to_string())
            .unwrap_or_else(|| full_path.clone());
        let mut hs = conn.prepare(
            "SELECT DISTINCT sha1 FROM file_locations WHERE source_id = ?1 AND path = ?2",
        )?;
        let sha1s = hs
            .query_map(rusqlite::params![source_id, rel], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        targets.push(ReclaimTarget {
            full_path,
            bytes,
            sha1s,
            is_archive: true,
        });
    }

    Ok(targets)
}

/// The source root path for a source id (used to recover a container's relative
/// path from its absolute path).
fn prefix_for(conn: &rusqlite::Connection, source_id: i64) -> Result<String> {
    let p: String = conn.query_row(
        "SELECT path FROM sources WHERE id = ?1",
        [source_id],
        |row| row.get(0),
    )?;
    Ok(p)
}

/// Confirm every content of `target` has an external copy that physically exists
/// on disk — the existence-verified-delete net. Returns false (skip) if any
/// external copy is missing, so a stale catalogue record can't cause loss.
fn external_copies_present(
    conn: &rusqlite::Connection,
    sources: &[files::Source],
    source_id: i64,
    target: &ReclaimTarget,
) -> Result<bool> {
    for sha1 in &target.sha1s {
        let locs = files::get_file_locations(conn, sha1)?;
        let mut ok = false;
        for l in locs {
            if l.source_id == source_id {
                continue; // a copy in the source we're reclaiming doesn't count
            }
            let root = sources
                .iter()
                .find(|s| s.id == l.source_id)
                .map(|s| s.path.trim_end_matches('/').to_string());
            let Some(root) = root else { continue };
            let abs = format!("{}/{}", root, l.path);
            if std::path::Path::new(&abs).exists() {
                ok = true;
                break;
            }
        }
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Split matched sources into those reclaim may empty and those it must refuse.
///
/// Reclaim deletes a source's files because a copy exists in **another** source —
/// cross-tree by construction. That is exactly what a `preserve` source forbids:
/// it must never lose content its own tree alone holds. So only `consume` sources
/// are reclaimable; preserve sources are refused. (Intra-tree dedup of a preserve
/// tree — dropping a duplicate where a copy survives in the *same* tree — is the
/// planner's job, not reclaim's.) See `decisions/source-disposition.md`.
fn partition_by_disposition<'a>(
    matched: &[&'a files::Source],
) -> (Vec<&'a files::Source>, Vec<&'a files::Source>) {
    matched
        .iter()
        .copied()
        .partition(|s| matches!(s.disposition, files::Disposition::Consume))
}

/// Run the reclaim command.
pub fn run(selector: Option<String>, execute: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let selector = selector.context(
        "reclaim needs a --source selector (a source id or a path substring) — \
         the source whose redundant files to reclaim",
    )?;

    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let sources = files::list_sources(conn)?;

    let matched: Vec<&files::Source> = sources
        .iter()
        .filter(|s| source_matches(s, &selector))
        .collect();
    if matched.is_empty() {
        println!("No source matches '{}'.", selector);
        return Ok(());
    }

    // A preserve source must never be emptied because a copy exists elsewhere;
    // reclaim only operates on consume sources.
    let (reclaimable, preserved) = partition_by_disposition(&matched);
    for s in &preserved {
        println!(
            "  Skipping '{}' — it is a preserve source; reclaim removes content a tree alone may hold.",
            s.path
        );
    }
    if reclaimable.is_empty() {
        println!("Nothing to reclaim: the matched source(s) are all preserve.");
        return Ok(());
    }

    let mut all: Vec<(i64, ReclaimTarget)> = Vec::new();
    for s in &reclaimable {
        for t in compute_reclaimable(conn, s.id)? {
            all.push((s.id, t));
        }
    }

    if all.is_empty() {
        println!("Nothing to reclaim: no fully-redundant files in the matched source(s).");
        return Ok(());
    }

    let total_bytes: i64 = all.iter().map(|(_, t)| t.bytes).sum();
    let loose = all.iter().filter(|(_, t)| !t.is_archive).count();
    let archives = all.len() - loose;

    println!(
        "Reclaimable: {} archive(s) + {} loose file(s), {} — every content is held in another source.",
        archives,
        loose,
        format_bytes(total_bytes.max(0) as u64)
    );

    if !execute {
        for (_, t) in all.iter().take(20) {
            println!("  would remove  {}", t.full_path);
        }
        if all.len() > 20 {
            println!("  … and {} more", all.len() - 20);
        }
        println!();
        println!("Dry run — nothing deleted. Re-run with --execute to free the space.");
        return Ok(());
    }

    // --execute: existence-verified hard delete, journaled.
    let mut removed: Vec<String> = Vec::new();
    let mut freed: i64 = 0;
    let mut skipped = 0usize;
    for (source_id, t) in &all {
        if !external_copies_present(conn, &sources, *source_id, t)? {
            eprintln!("  SKIP (external copy missing on disk): {}", t.full_path);
            skipped += 1;
            continue;
        }
        match std::fs::remove_file(&t.full_path) {
            Ok(()) => {
                // Drop the catalogue rows for the removed file.
                if let Some((sid, rel)) = resolve_in_sources(&sources, &t.full_path) {
                    conn.execute(
                        "DELETE FROM file_locations WHERE source_id = ?1 AND path = ?2",
                        rusqlite::params![sid, rel],
                    )?;
                }
                removed.push(t.full_path.clone());
                freed += t.bytes;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some((sid, rel)) = resolve_in_sources(&sources, &t.full_path) {
                    conn.execute(
                        "DELETE FROM file_locations WHERE source_id = ?1 AND path = ?2",
                        rusqlite::params![sid, rel],
                    )?;
                }
            }
            Err(e) => eprintln!("  ERROR deleting {}: {:#}", t.full_path, e),
        }
    }

    // Journal the run for audit (hard delete is irreversible).
    let logs_dir = get_data_dir(data_dir)?.join("objects/reclaim-logs");
    std::fs::create_dir_all(&logs_dir).ok();
    let log_path = logs_dir.join(format!("reclaim-{}.txt", removed.len()));
    std::fs::write(&log_path, removed.join("\n")).ok();

    println!();
    println!(
        "Reclaimed {} file(s), freed {}{}.",
        removed.len(),
        format_bytes(freed.max(0) as u64),
        if skipped > 0 {
            format!(" ({} skipped — external copy missing)", skipped)
        } else {
            String::new()
        }
    );
    println!("Audit log: {}", log_path.display());
    Ok(())
}

/// Format bytes as a human-readable string.
fn format_bytes(bytes: u64) -> String {
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
    use crate::db::Database;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn source(id: i64, path: &str, disposition: files::Disposition) -> files::Source {
        files::Source {
            id,
            path: path.to_string(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
            disposition,
        }
    }

    // Reclaim's model — delete here because a copy exists in another source — is
    // forbidden for a preserve tree, so only consume sources are reclaimable.
    #[test]
    fn reclaim_refuses_preserve_sources() {
        let staging = source(1, "/ToSort", files::Disposition::Consume);
        let master = source(2, "/Master", files::Disposition::Preserve);
        let matched = vec![&staging, &master];

        let (reclaimable, preserved) = partition_by_disposition(&matched);
        assert_eq!(
            reclaimable.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1],
            "only the consume source is reclaimable"
        );
        assert_eq!(
            preserved.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![2],
            "the preserve source is refused"
        );
    }

    // A selector matching only preserve sources reclaims nothing.
    #[test]
    fn reclaim_partition_is_empty_when_all_preserve() {
        let a = source(1, "/MasterA", files::Disposition::Preserve);
        let b = source(2, "/MasterB", files::Disposition::Preserve);
        let matched = vec![&a, &b];
        let (reclaimable, preserved) = partition_by_disposition(&matched);
        assert!(reclaimable.is_empty());
        assert_eq!(preserved.len(), 2);
    }

    #[test]
    fn reclaims_fully_redundant_containers_keeps_unique_ones() {
        let db = setup();
        let conn = db.conn();
        // Source 1 = staging (ToSort), source 2 = library.
        let staging = files::add_source(conn, "/ToSort", false).unwrap();
        let library = files::add_source(conn, "/Library", false).unwrap();

        // Content A and B are held in BOTH; content C only in staging.
        for (sha, size) in [("AAA", 10), ("BBB", 20), ("CCC", 30)] {
            files::upsert_file(conn, sha, None, None, None, size).unwrap();
        }
        // staging/redundant.zip holds A + B — both also in the library.
        files::upsert_file_location(conn, "AAA", staging, "redundant.zip", Some("a.rom")).unwrap();
        files::upsert_file_location(conn, "BBB", staging, "redundant.zip", Some("b.rom")).unwrap();
        files::upsert_file_location(conn, "AAA", library, "g1.zip", Some("a.rom")).unwrap();
        files::upsert_file_location(conn, "BBB", library, "g1.zip", Some("b.rom")).unwrap();
        // staging/unique.zip holds A (redundant) + C (held nowhere else).
        files::upsert_file_location(conn, "AAA", staging, "unique.zip", Some("a.rom")).unwrap();
        files::upsert_file_location(conn, "CCC", staging, "unique.zip", Some("c.rom")).unwrap();
        // staging/loose.rom is content B (redundant) as a loose file.
        files::upsert_file_location(conn, "BBB", staging, "loose.rom", None).unwrap();

        let targets = compute_reclaimable(conn, staging).unwrap();
        let paths: Vec<&str> = targets.iter().map(|t| t.full_path.as_str()).collect();

        assert!(
            paths.contains(&"/ToSort/redundant.zip"),
            "container whose every entry is held elsewhere is reclaimable"
        );
        assert!(
            paths.contains(&"/ToSort/loose.rom"),
            "loose file held elsewhere is reclaimable"
        );
        assert!(
            !paths.contains(&"/ToSort/unique.zip"),
            "container with a unique entry (C) is NOT reclaimable"
        );
        // redundant.zip reports both entries' bytes for the freed total.
        let redundant = targets
            .iter()
            .find(|t| t.full_path == "/ToSort/redundant.zip")
            .unwrap();
        assert_eq!(redundant.bytes, 30);
        assert_eq!(redundant.sha1s.len(), 2);
    }
}
