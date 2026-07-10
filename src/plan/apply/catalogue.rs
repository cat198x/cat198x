use anyhow::Result;
use rusqlite::Connection;

use crate::db::files::{self, Source};
use crate::plan::OperationKind;

/// Update the file catalogue to match a completed operation, so a re-plan
/// converges without a re-scan: a move/relocate updates the file's recorded
/// location, a quarantine/delete removes it, a copy/repack records the new copy.
/// Paths outside any registered source can't be recorded (the file simply leaves
/// the catalogue's view); the library destination is a source, so the common
/// cases resolve.
pub(super) fn sync_catalogue_after(
    conn: &Connection,
    sources: &[Source],
    kind: &OperationKind,
) -> Result<()> {
    match kind {
        OperationKind::Move { source, dest, .. } => {
            if source.archive_path.is_some() {
                // A copy extracted from an archive to a loose dest; source kept.
                if let Some((nsrc, nrel)) = files::resolve_in_sources(sources, dest) {
                    files::upsert_file_location(conn, &source.sha1, nsrc, &nrel, None)?;
                }
            } else {
                relocate_or_drop(conn, sources, &source.path, dest)?;
            }
        }
        OperationKind::Relocate { source, dest, .. } => {
            relocate_or_drop(conn, sources, source, dest)?;
        }
        OperationKind::Copy { source, dest, .. } => {
            if let Some((nsrc, nrel)) = files::resolve_in_sources(sources, dest) {
                files::upsert_file_location(conn, &source.sha1, nsrc, &nrel, None)?;
            }
        }
        OperationKind::Repack {
            sources: entries,
            dest,
            move_sources,
            ..
        } => {
            if let Some((nsrc, nrel)) = files::resolve_in_sources(sources, dest) {
                for e in entries {
                    files::upsert_file_location(
                        conn,
                        &e.sha1,
                        nsrc,
                        &nrel,
                        e.entry_name.as_deref(),
                    )?;
                }
            }
            // Move mode deleted the loose sources on disk; drop their catalogued
            // locations too (archive-member sources are left in place).
            if *move_sources {
                for e in entries {
                    if e.archive_path.is_none()
                        && let Some((src, rel)) = files::resolve_in_sources(sources, &e.path)
                    {
                        files::remove_locations_at(conn, src, &rel)?;
                    }
                }
            }
        }
        OperationKind::Quarantine { path, .. } | OperationKind::Delete { path, .. } => {
            if let Some((src, rel)) = files::resolve_in_sources(sources, path) {
                files::remove_locations_at(conn, src, &rel)?;
            }
        }
    }
    Ok(())
}

/// Move a file's catalogued location(s) from `old_abs` to `new_abs`, or drop them
/// if the destination is outside every registered source.
fn relocate_or_drop(
    conn: &Connection,
    sources: &[Source],
    old_abs: &str,
    new_abs: &str,
) -> Result<()> {
    match (
        files::resolve_in_sources(sources, old_abs),
        files::resolve_in_sources(sources, new_abs),
    ) {
        (Some((osrc, orel)), Some((nsrc, nrel))) => {
            files::relocate_locations(conn, osrc, &orel, nsrc, &nrel)?;
        }
        (Some((osrc, orel)), None) => {
            files::remove_locations_at(conn, osrc, &orel)?;
        }
        _ => {}
    }
    Ok(())
}
