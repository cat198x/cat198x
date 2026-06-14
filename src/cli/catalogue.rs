//! `catalogue-placements` command — record a completed plan's library placements.
//!
//! Closes the Q6 convergence gap. The destination library is not a registered
//! source, so the apply-time catalogue sync cannot record the files it places
//! there (`relocate_or_drop` drops the source location and records nothing), and
//! every re-plan re-lists the work — re-transferring shared copies over the
//! network mount. This command registers the library root as a source and
//! catalogues every file a completed plan placed there, taking each placement's
//! SHA1 from the plan's own operations — which the apply already verified — so
//! nothing is re-hashed. Afterwards re-plans converge, and because the library
//! is now a registered source, future applies record their own placements.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::{get_data_dir, open_database};
use crate::config::Config;
use crate::db::files::{self, Source};
use crate::plan::{OperationKind, OperationStatus, Plan};

/// Record, into the catalogue, every library file a completed plan placed.
///
/// `plan_filter`, when set, restricts the scan to saved plans whose file name
/// contains that substring (e.g. a plan hash) — so a single reorg's placements
/// can be catalogued without touching unrelated history.
pub fn run(dry_run: bool, plan_filter: Option<String>, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    // The library root every placement lives under — the planning default dest.
    let config_path = get_data_dir(data_dir.clone())?.join("config.toml");
    let config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_default()
    } else {
        Config::default()
    };
    let dest_root = config
        .default_dest_path
        .clone()
        .context("no default_dest_path configured — set it with `cat198x config set-default dest_path <path>`")?;
    let dest_root = dest_root.trim_end_matches('/').to_string();

    // Wrap the writes in a single transaction — hundreds of thousands of
    // per-row commits would otherwise take many minutes. The same connection
    // sees the uncommitted source registration below.
    let tx = if dry_run {
        None
    } else {
        Some(conn.unchecked_transaction()?)
    };
    let wconn: &rusqlite::Connection = match &tx {
        Some(t) => t,
        None => conn,
    };

    // Register the library root as a source (idempotent) so placements have a
    // source_id and future applies converge. Simulated in a dry run.
    let lib_id = match files::get_source_by_path(wconn, &dest_root)? {
        Some(s) => s.id,
        None if !dry_run => files::add_source(wconn, &dest_root, false)?,
        None => -1,
    };
    // Rebuild this library source's locations from scratch, so a re-run (or a
    // resumed/partial earlier run) can't accumulate duplicate loose-file rows —
    // the unique index does not dedupe rows whose archive_path is NULL. Only the
    // library source's own rows are touched.
    if !dry_run {
        wconn.execute("DELETE FROM file_locations WHERE source_id = ?1", [lib_id])?;
    }
    // The source list resolve_in_sources needs — guaranteed to include the
    // library root even in a dry run, so the preview can resolve dest paths.
    let mut sources = files::list_sources(wconn)?;
    if !sources
        .iter()
        .any(|s| s.path.trim_end_matches('/') == dest_root)
    {
        sources.push(Source {
            id: lib_id,
            path: dest_root.clone(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
        });
    }

    let mut placements = 0usize;
    let mut games = 0usize;
    let mut skipped_relocates = 0usize;

    // Completed operations may live in any saved plan (a re-plan is all-pending),
    // so scan them all (or just the filtered one); upserts make this idempotent
    // across overlapping plans.
    for plan in load_plans(data_dir.clone(), plan_filter.as_deref())? {
        for op in &plan.operations {
            if op.status != OperationStatus::Completed {
                continue;
            }
            match &op.kind {
                OperationKind::Copy { source, dest, .. }
                | OperationKind::Move { source, dest, .. } => {
                    if let Some((sid, rel)) = files::resolve_in_sources(&sources, dest) {
                        if !dry_run {
                            files::upsert_file_location(wconn, &source.sha1, sid, &rel, None)?;
                        }
                        placements += 1;
                        games += 1;
                    }
                }
                OperationKind::Repack {
                    sources: entries,
                    dest,
                    ..
                } => {
                    if let Some((sid, rel)) = files::resolve_in_sources(&sources, dest) {
                        for e in entries {
                            if !dry_run {
                                files::upsert_file_location(
                                    wconn,
                                    &e.sha1,
                                    sid,
                                    &rel,
                                    e.entry_name.as_deref(),
                                )?;
                            }
                            placements += 1;
                        }
                        games += 1;
                    }
                }
                OperationKind::Relocate { .. } => {
                    // A relocate carries no content hash; cataloguing it would
                    // need the archive's entries. (None completed in the run this
                    // fixes.) Counted for transparency.
                    skipped_relocates += 1;
                }
                OperationKind::Delete { .. } | OperationKind::Quarantine { .. } => {}
            }
        }
    }

    if let Some(tx) = tx {
        tx.commit()?;
    }

    let verb = if dry_run {
        "Would catalogue"
    } else {
        "Catalogued"
    };
    println!("{verb} {placements} placement(s) from {games} completed game(s) under {dest_root}");
    if skipped_relocates > 0 {
        println!(
            "  {skipped_relocates} completed relocate(s) skipped (no content hash in the op)."
        );
    }
    if dry_run {
        println!();
        println!("Preview only. Re-run without --dry-run to write these to the catalogue.");
    } else {
        println!("Library registered as a source; a re-plan will now converge.");
    }
    Ok(())
}

/// Load saved plans under `objects/plans`, optionally restricted to those whose
/// file name contains `filter`. A plan that fails to parse is skipped rather than
/// aborting the whole pass.
fn load_plans(data_dir: Option<PathBuf>, filter: Option<&str>) -> Result<Vec<Plan>> {
    let plans_dir = get_data_dir(data_dir)?.join("objects/plans");
    if !plans_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut plans = Vec::new();
    for entry in std::fs::read_dir(&plans_dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.extension().map(|e| e == "json").unwrap_or(false)
            && filter.is_none_or(|f| name.contains(f))
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(plan) = serde_json::from_str::<Plan>(&text)
        {
            plans.push(plan);
        }
    }
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Plan, SourceRef};

    fn src(sha1: &str) -> SourceRef {
        SourceRef {
            path: format!("/ToSort/{sha1}.zip"),
            archive_path: Some("e.rom".to_string()),
            sha1: sha1.to_string(),
            entry_name: Some("e.rom".to_string()),
        }
    }

    #[test]
    fn resolve_records_loose_and_archive_placements_under_library() {
        // A library source plus one completed loose copy and one completed
        // repack: cataloguing should resolve both dests under the library and
        // record the loose file and each archive entry.
        let sources = vec![Source {
            id: 7,
            path: "/lib/ROMs".to_string(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
        }];

        // Loose copy dest.
        let (sid, rel) =
            files::resolve_in_sources(&sources, "/lib/ROMs/MAME/game/rom.bin").unwrap();
        assert_eq!(sid, 7);
        assert_eq!(rel, "MAME/game/rom.bin");

        // Archive (repack) dest.
        let (sid2, rel2) =
            files::resolve_in_sources(&sources, "/lib/ROMs/MAME/Software List/g.zip").unwrap();
        assert_eq!(sid2, 7);
        assert_eq!(rel2, "MAME/Software List/g.zip");

        // A dest outside the library does not resolve (would not be catalogued).
        assert!(files::resolve_in_sources(&sources, "/elsewhere/x.bin").is_none());
    }

    #[test]
    fn only_completed_ops_are_counted() {
        // Sanity: a plan's completed vs pending split is what the command keys on.
        let mut plan = Plan::new("h".to_string());
        plan.add_copy(src("AAA"), "/lib/ROMs/g/a.bin".to_string(), 1);
        plan.add_copy(src("BBB"), "/lib/ROMs/g/b.bin".to_string(), 1);
        // Mark the first completed, leave the second pending.
        plan.operations[0].status = OperationStatus::Completed;
        let completed = plan
            .operations
            .iter()
            .filter(|o| o.status == OperationStatus::Completed)
            .count();
        assert_eq!(completed, 1);
    }
}
