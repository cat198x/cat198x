use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::dats::{self, MergeMode};
use crate::db::{collections, files};

/// Completeness of one collection against its active DAT.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionStatus {
    pub name: String,
    /// The active version label, or `None` when the collection has no active version.
    pub version: Option<String>,
    pub total_games: usize,
    pub total_roms: usize,
    pub have_roms: usize,
    pub missing_roms: usize,
    pub completion_pct: f64,
    pub nodump_roms: usize,
    pub bios_sets: usize,
    pub device_sets: usize,
}

/// A registered collection.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionInfo {
    pub name: String,
    pub source_type: String,
    pub has_active_version: bool,
    /// The collection's full library path — the whole tree it sits in, e.g.
    /// `TOSEC/Acorn/Archimedes/Games/[ADF]` — set by recursive `dat add`. Falls
    /// back to the collection name when it has no active version or no recorded
    /// path. A caller groups the catalogue's thousands of collections by walking
    /// this path: the first segment is the set, the rest the manufacturer /
    /// system / category tree beneath it.
    pub node_path: String,
}

/// A registered source directory.
#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub id: i64,
    pub path: String,
    pub last_scanned: Option<String>,
}

/// Collection completeness, optionally filtered to one collection by name.
///
/// A collection with no active version is reported with `version: None` and zero
/// counts rather than omitted, so a caller sees every registered collection.
pub fn collection_status(
    conn: &Connection,
    collection: Option<&str>,
    mode: MergeMode,
) -> Result<Vec<CollectionStatus>> {
    let mut out = Vec::new();
    for coll in collections::list_collections(conn)? {
        if let Some(name) = collection
            && coll.name != name
        {
            continue;
        }
        let Some(version) = collections::get_active_version(conn, coll.id)? else {
            out.push(CollectionStatus {
                name: coll.name,
                version: None,
                total_games: 0,
                total_roms: 0,
                have_roms: 0,
                missing_roms: 0,
                completion_pct: 0.0,
                nodump_roms: 0,
                bios_sets: 0,
                device_sets: 0,
            });
            continue;
        };
        // exclude_mechanical by default, matching the `status` command.
        let stats = dats::calculate_merge_mode_stats(conn, version.id, mode, true)?;
        let completion_pct = if stats.total_roms > 0 {
            (stats.have_roms as f64 / stats.total_roms as f64) * 100.0
        } else {
            0.0
        };
        out.push(CollectionStatus {
            name: coll.name,
            version: Some(version.version),
            total_games: stats.total_games,
            total_roms: stats.total_roms,
            have_roms: stats.have_roms,
            missing_roms: stats.total_roms.saturating_sub(stats.have_roms),
            completion_pct,
            nodump_roms: stats.nodump_roms,
            bios_sets: stats.bios_sets,
            device_sets: stats.device_sets,
        });
    }
    Ok(out)
}

/// Every registered collection, with whether it has an active version and the
/// set it rolls up under.
pub fn list_collections(conn: &Connection) -> Result<Vec<CollectionInfo>> {
    let mut out = Vec::new();
    for coll in collections::list_collections(conn)? {
        let version = collections::get_active_version(conn, coll.id)?;
        // Full library path (the tree set by recursive `dat add`); fall back to
        // the collection name when there's no active version or recorded path.
        let node_path = match &version {
            Some(v) => dats::primary_node_path(conn, v.id)?.unwrap_or_else(|| coll.name.clone()),
            None => coll.name.clone(),
        };
        out.push(CollectionInfo {
            name: coll.name,
            source_type: coll.source_type,
            has_active_version: version.is_some(),
            node_path,
        });
    }
    Ok(out)
}

/// Every registered source directory.
pub fn list_sources(conn: &Connection) -> Result<Vec<SourceInfo>> {
    Ok(files::list_sources(conn)?
        .into_iter()
        .map(|s| SourceInfo {
            id: s.id,
            path: s.path,
            last_scanned: s.last_scanned,
        })
        .collect())
}
