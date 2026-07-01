use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::cli::audit_log::write_hard_delete_log;
use crate::db::files::{self, Source};

use super::analysis::ReclaimTarget;

pub(super) struct ExecutionReport {
    pub(super) removed_count: usize,
    pub(super) freed_bytes: i64,
    pub(super) skipped: usize,
    pub(super) log_path: PathBuf,
}

/// Verify external copies, delete reclaimable files, update catalogue rows, and
/// write an audit log for the irreversible hard-delete operation.
pub(super) fn execute_reclaim(
    conn: &rusqlite::Connection,
    sources: &[Source],
    targets: &[(i64, ReclaimTarget)],
    data_dir: Option<PathBuf>,
) -> Result<ExecutionReport> {
    let mut removed: Vec<String> = Vec::new();
    let mut freed_bytes: i64 = 0;
    let mut skipped = 0usize;

    for (source_id, target) in targets {
        if !external_copies_present(conn, sources, *source_id, target)? {
            eprintln!(
                "  SKIP (external copy missing on disk): {}",
                target.full_path
            );
            skipped += 1;
            continue;
        }
        match std::fs::remove_file(&target.full_path) {
            Ok(()) => {
                remove_catalogue_rows(conn, sources, &target.full_path)?;
                removed.push(target.full_path.clone());
                freed_bytes += target.bytes;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                remove_catalogue_rows(conn, sources, &target.full_path)?;
            }
            Err(e) => eprintln!("  ERROR deleting {}: {:#}", target.full_path, e),
        }
    }

    let log_path = write_hard_delete_log(data_dir, "reclaim-logs", "reclaim", &removed)?;

    Ok(ExecutionReport {
        removed_count: removed.len(),
        freed_bytes,
        skipped,
        log_path,
    })
}

/// Confirm every content of `target` has an external copy that physically exists
/// on disk — the existence-verified-delete net. Returns false (skip) if any
/// external copy is missing, so a stale catalogue record can't cause loss.
fn external_copies_present(
    conn: &rusqlite::Connection,
    sources: &[Source],
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
            let abs = Path::new(&root).join(&l.path);
            if catalogued_location_exists(&abs, l.archive_path.as_deref()) {
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

fn catalogued_location_exists(path: &Path, archive_path: Option<&str>) -> bool {
    match archive_path {
        Some(entry_path) => crate::archive::extract_archive_entry(path, entry_path).is_ok(),
        None => path.exists(),
    }
}

fn remove_catalogue_rows(
    conn: &rusqlite::Connection,
    sources: &[Source],
    full_path: &str,
) -> Result<()> {
    if let Some((sid, rel)) = files::resolve_in_sources(sources, full_path) {
        files::remove_locations_at(conn, sid, &rel)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn path_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn target(full_path: &Path, bytes: i64, sha1: &str) -> ReclaimTarget {
        ReclaimTarget {
            full_path: path_string(full_path),
            bytes,
            sha1s: vec![sha1.to_string()],
            is_archive: false,
        }
    }

    fn write_zip_entry(zip_path: &Path, entry_name: &str, content: &[u8]) {
        use std::io::Write;

        let file = std::fs::File::create(zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file(entry_name, options).unwrap();
        zip.write_all(content).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn execute_reclaim_deletes_verified_target_and_catalogue_rows() {
        let db = setup();
        let conn = db.conn();
        let data_dir = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let library = tempfile::tempdir().unwrap();
        let staging_file = staging.path().join("redundant.rom");
        let library_file = library.path().join("copy.rom");
        std::fs::write(&staging_file, b"staging").unwrap();
        std::fs::write(&library_file, b"library").unwrap();

        let staging_id = files::add_source(conn, &path_string(staging.path()), false).unwrap();
        let library_id = files::add_source(conn, &path_string(library.path()), false).unwrap();
        files::upsert_file(conn, "AAA", None, None, None, 7).unwrap();
        files::upsert_file_location(conn, "AAA", staging_id, "redundant.rom", None).unwrap();
        files::upsert_file_location(conn, "AAA", library_id, "copy.rom", None).unwrap();
        let sources = files::list_sources(conn).unwrap();

        let report = execute_reclaim(
            conn,
            &sources,
            &[(staging_id, target(&staging_file, 7, "AAA"))],
            Some(data_dir.path().to_path_buf()),
        )
        .unwrap();

        assert_eq!(report.removed_count, 1);
        assert_eq!(report.freed_bytes, 7);
        assert_eq!(report.skipped, 0);
        assert!(!staging_file.exists());
        let locations = files::get_file_locations(conn, "AAA").unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].source_id, library_id);
        assert_eq!(
            std::fs::read_to_string(report.log_path).unwrap(),
            path_string(&staging_file)
        );
    }

    #[test]
    fn execute_reclaim_skips_when_external_copy_is_missing_on_disk() {
        let db = setup();
        let conn = db.conn();
        let data_dir = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let library = tempfile::tempdir().unwrap();
        let staging_file = staging.path().join("redundant.rom");
        std::fs::write(&staging_file, b"staging").unwrap();

        let staging_id = files::add_source(conn, &path_string(staging.path()), false).unwrap();
        let library_id = files::add_source(conn, &path_string(library.path()), false).unwrap();
        files::upsert_file(conn, "AAA", None, None, None, 7).unwrap();
        files::upsert_file_location(conn, "AAA", staging_id, "redundant.rom", None).unwrap();
        files::upsert_file_location(conn, "AAA", library_id, "missing.rom", None).unwrap();
        let sources = files::list_sources(conn).unwrap();

        let report = execute_reclaim(
            conn,
            &sources,
            &[(staging_id, target(&staging_file, 7, "AAA"))],
            Some(data_dir.path().to_path_buf()),
        )
        .unwrap();

        assert_eq!(report.removed_count, 0);
        assert_eq!(report.freed_bytes, 0);
        assert_eq!(report.skipped, 1);
        assert!(staging_file.exists());
        assert_eq!(files::get_file_locations(conn, "AAA").unwrap().len(), 2);
        assert_eq!(std::fs::read_to_string(report.log_path).unwrap(), "");
    }

    #[test]
    fn execute_reclaim_skips_when_external_archive_entry_is_missing() {
        let db = setup();
        let conn = db.conn();
        let data_dir = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let library = tempfile::tempdir().unwrap();
        let staging_file = staging.path().join("redundant.rom");
        let library_archive = library.path().join("copy.zip");
        std::fs::write(&staging_file, b"staging").unwrap();
        write_zip_entry(&library_archive, "other.rom", b"different");

        let staging_id = files::add_source(conn, &path_string(staging.path()), false).unwrap();
        let library_id = files::add_source(conn, &path_string(library.path()), false).unwrap();
        files::upsert_file(conn, "AAA", None, None, None, 7).unwrap();
        files::upsert_file_location(conn, "AAA", staging_id, "redundant.rom", None).unwrap();
        files::upsert_file_location(conn, "AAA", library_id, "copy.zip", Some("missing.rom"))
            .unwrap();
        let sources = files::list_sources(conn).unwrap();

        let report = execute_reclaim(
            conn,
            &sources,
            &[(staging_id, target(&staging_file, 7, "AAA"))],
            Some(data_dir.path().to_path_buf()),
        )
        .unwrap();

        assert_eq!(report.removed_count, 0);
        assert_eq!(report.freed_bytes, 0);
        assert_eq!(report.skipped, 1);
        assert!(staging_file.exists());
        assert_eq!(files::get_file_locations(conn, "AAA").unwrap().len(), 2);
        assert_eq!(std::fs::read_to_string(report.log_path).unwrap(), "");
    }

    #[test]
    fn execute_reclaim_removes_stale_catalogue_row_for_missing_target() {
        let db = setup();
        let conn = db.conn();
        let data_dir = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let library = tempfile::tempdir().unwrap();
        let stale_file = staging.path().join("already-gone.rom");
        let library_file = library.path().join("copy.rom");
        std::fs::write(&library_file, b"library").unwrap();

        let staging_id = files::add_source(conn, &path_string(staging.path()), false).unwrap();
        let library_id = files::add_source(conn, &path_string(library.path()), false).unwrap();
        files::upsert_file(conn, "AAA", None, None, None, 7).unwrap();
        files::upsert_file_location(conn, "AAA", staging_id, "already-gone.rom", None).unwrap();
        files::upsert_file_location(conn, "AAA", library_id, "copy.rom", None).unwrap();
        let sources = files::list_sources(conn).unwrap();

        let report = execute_reclaim(
            conn,
            &sources,
            &[(staging_id, target(&stale_file, 7, "AAA"))],
            Some(data_dir.path().to_path_buf()),
        )
        .unwrap();

        assert_eq!(report.removed_count, 0);
        assert_eq!(report.freed_bytes, 0);
        assert_eq!(report.skipped, 0);
        let locations = files::get_file_locations(conn, "AAA").unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].source_id, library_id);
        assert_eq!(std::fs::read_to_string(report.log_path).unwrap(), "");
    }
}
