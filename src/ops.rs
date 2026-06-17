//! The shared operation surface.
//!
//! Each Cat198x operation is defined once here as a typed request → response, so
//! the CLI, the `cat198x mcp` server, and the Tauri UI are thin adapters over one
//! audited core rather than parallel implementations. See
//! `decisions/agent-native-surface-and-ui.md`: any action an adapter offers is,
//! by construction, an operation every other adapter can invoke.
//!
//! Functions here are **silent** — they return data and never print — so the
//! adapter owns all output. That is load-bearing for the MCP stdio server, whose
//! stdout is the JSON-RPC transport: a stray `println!` in an operation would
//! corrupt the protocol stream.
//!
//! This is the read-only foundation — collection status, the saved plan-as-diff,
//! and collection/source listings, the operations the first UI slice needs.
//! Mutating operations (apply, reclaim, clean-superseded) join it behind a
//! structured-progress-event design in a follow-up.

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::dats::{self, MergeMode};
use crate::db::{collections, files};
use crate::plan::Plan;

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

/// Every registered collection, with whether it has an active version.
pub fn list_collections(conn: &Connection) -> Result<Vec<CollectionInfo>> {
    let mut out = Vec::new();
    for coll in collections::list_collections(conn)? {
        let has_active_version = collections::get_active_version(conn, coll.id)?.is_some();
        out.push(CollectionInfo {
            name: coll.name,
            source_type: coll.source_type,
            has_active_version,
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

/// The most recent saved plan — the plan-as-diff the UI renders — or `None` when
/// no plan has been generated. Reads the newest plan JSON under
/// `<data_dir>/objects/plans`; the plan already *is* the diff, so no reconcile
/// model is needed (see the decision record).
pub fn latest_plan(data_dir: &Path) -> Result<Option<Plan>> {
    let plans_dir = data_dir.join("objects/plans");
    if !plans_dir.is_dir() {
        return Ok(None);
    }
    let mut latest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(&plans_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Ok(modified) = entry.metadata().and_then(|m| m.modified())
        {
            let newer = match &latest {
                Some((_, prev)) => modified > *prev,
                None => true,
            };
            if newer {
                latest = Some((path, modified));
            }
        }
    }
    match latest {
        Some((path, _)) => {
            let contents = std::fs::read_to_string(&path)?;
            Ok(Some(serde_json::from_str(&contents)?))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, collections, dats};

    #[test]
    fn collection_status_reports_completeness_and_no_active_version() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        // One collection with an active version holding one of two ROMs…
        let c1 = collections::create_collection(conn, "NES", "nointro").unwrap();
        let v1 = collections::add_version(conn, c1, "v1", "/d/nes.dat", true).unwrap();
        let node = dats::create_node(conn, v1, None, "NES", "dat", "NES").unwrap();
        let g = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g, "a.nes", 10, Some("AAA"), None, None, "good", None).unwrap();
        dats::create_rom(conn, g, "b.nes", 10, Some("BBB"), None, None, "good", None).unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();

        // …and one collection with no active version.
        collections::create_collection(conn, "Empty", "nointro").unwrap();

        let all = collection_status(conn, None, MergeMode::NonMerged).unwrap();
        assert_eq!(all.len(), 2);

        let nes = all.iter().find(|s| s.name == "NES").unwrap();
        assert_eq!(nes.version.as_deref(), Some("v1"));
        assert_eq!(nes.total_roms, 2);
        assert_eq!(nes.have_roms, 1);
        assert_eq!(nes.missing_roms, 1);
        assert!((nes.completion_pct - 50.0).abs() < 1e-9);

        let empty = all.iter().find(|s| s.name == "Empty").unwrap();
        assert_eq!(empty.version, None);
        assert_eq!(empty.total_roms, 0);

        // Filtering returns just the requested collection.
        let just_nes = collection_status(conn, Some("NES"), MergeMode::NonMerged).unwrap();
        assert_eq!(just_nes.len(), 1);
        assert_eq!(just_nes[0].name, "NES");
    }

    #[test]
    fn latest_plan_is_none_without_a_saved_plan() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(latest_plan(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn latest_plan_reads_the_newest_saved_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        // A round-tripped Plan deserializes back through latest_plan.
        let plan = Plan::new("deadbeefdeadbeef".to_string());
        let json = serde_json::to_string_pretty(&plan).unwrap();
        std::fs::write(plans.join("deadbeefdeadbeef.json"), json).unwrap();

        let loaded = latest_plan(tmp.path()).unwrap().expect("a plan");
        assert_eq!(loaded.state_hash, "deadbeefdeadbeef");
    }
}
