use anyhow::Result;

use crate::db::files;

/// A file (loose file or whole archive container) that can be reclaimed.
#[derive(Debug, Clone)]
pub(super) struct ReclaimTarget {
    /// Absolute path to delete.
    pub(super) full_path: String,
    /// Bytes freed by deleting it.
    pub(super) bytes: i64,
    /// Distinct contents (sha1) it holds — for the existence-verified delete.
    pub(super) sha1s: Vec<String>,
    /// `true` for a whole archive container, `false` for a loose file.
    pub(super) is_archive: bool,
}

#[derive(Debug)]
pub(super) struct ReclaimReport {
    pub(super) targets: Vec<(i64, ReclaimTarget)>,
    pub(super) total_bytes: i64,
    pub(super) loose_count: usize,
    pub(super) archive_count: usize,
}

/// Whether a `--source` selector picks this source: a numeric selector is a
/// source id (exact); anything else matches as a path substring.
pub(super) fn source_matches(source: &files::Source, selector: &str) -> bool {
    match selector.parse::<i64>() {
        Ok(id) => source.id == id,
        Err(_) => source.path.contains(selector),
    }
}

pub(super) fn analyze_reclaimable(
    conn: &rusqlite::Connection,
    sources: &[&files::Source],
) -> Result<ReclaimReport> {
    let mut targets: Vec<(i64, ReclaimTarget)> = Vec::new();
    for source in sources {
        for target in compute_reclaimable(conn, source.id)? {
            targets.push((source.id, target));
        }
    }

    let total_bytes = targets.iter().map(|(_, target)| target.bytes).sum();
    let loose_count = targets
        .iter()
        .filter(|(_, target)| !target.is_archive)
        .count();
    let archive_count = targets.len() - loose_count;

    Ok(ReclaimReport {
        targets,
        total_bytes,
        loose_count,
        archive_count,
    })
}

/// The files in `source_id` whose every content is also held in another source.
pub(super) fn compute_reclaimable(
    conn: &rusqlite::Connection,
    source_id: i64,
) -> Result<Vec<ReclaimTarget>> {
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

/// Split matched sources into those reclaim may empty and those it must refuse.
///
/// Reclaim deletes a source's files because a copy exists in **another** source —
/// cross-tree by construction. That is exactly what a `preserve` source forbids:
/// it must never lose content its own tree alone holds. So only `consume` sources
/// are reclaimable; preserve sources are refused. (Intra-tree dedup of a preserve
/// tree — dropping a duplicate where a copy survives in the *same* tree — is the
/// planner's job, not reclaim's.) See `decisions/source-disposition.md`.
pub(super) fn partition_by_disposition<'a>(
    matched: &[&'a files::Source],
) -> (Vec<&'a files::Source>, Vec<&'a files::Source>) {
    matched
        .iter()
        .copied()
        .partition(|s| matches!(s.disposition, files::Disposition::Consume))
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
    fn source_matches_numeric_selector_by_id_only() {
        let source = source(42, "/roms/42", files::Disposition::Consume);

        assert!(source_matches(&source, "42"));
        assert!(!source_matches(&source, "41"));
    }

    #[test]
    fn source_matches_non_numeric_selector_by_path_substring() {
        let source = source(42, "/roms/ToSort/NES", files::Disposition::Consume);

        assert!(source_matches(&source, "ToSort"));
        assert!(!source_matches(&source, "Master"));
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

        let staging_source = source(staging, "/ToSort", files::Disposition::Consume);
        let report = analyze_reclaimable(conn, &[&staging_source]).unwrap();
        assert_eq!(report.targets.len(), 2);
        assert_eq!(report.total_bytes, 50);
        assert_eq!(report.archive_count, 1);
        assert_eq!(report.loose_count, 1);
    }
}
