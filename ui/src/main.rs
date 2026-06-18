//! Cat198x desktop UI (Tauri) — the read-only status + plan-diff slice.
//!
//! A thin client over the shared operation surface (`cat198x::ops`): the Tauri
//! commands carry no logic of their own, they invoke the same operations the CLI
//! formats and the `cat198x mcp` server exposes. See
//! `decisions/agent-native-surface-and-ui.md` — one surface, three adapters.
//!
//! This first slice is read-only: collection completeness and the saved
//! plan-as-diff. Completeness is fetched per collection (`collections` lists the
//! names instantly; `status_one` computes one collection's numbers), so the UI
//! shows the list immediately and fills each row's stats concurrently rather
//! than blocking on the whole catalogue. Mutating actions (apply, reclaim,
//! clean-superseded) land once the operation surface grows progress events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::Path;

use cat198x::db::Database;
use cat198x::db::dats::MergeMode;
use cat198x::ops::{self, CollectionInfo, CollectionStatus};
use cat198x::plan::{Operation, PlanSummary};

/// How many of a plan's operations the UI receives. A real plan can hold tens of
/// thousands; the diff is for review, and the frontend caps what it draws, so we
/// send a bounded prefix plus the true total rather than a multi-megabyte payload.
const MAX_PLAN_OPS: usize = 2000;

/// Map a merge-mode string from the UI to the enum, defaulting to non-merged —
/// the same mapping the other adapters use.
fn parse_merge_mode(s: Option<&str>) -> MergeMode {
    match s {
        Some("split") => MergeMode::Split,
        Some("merged") => MergeMode::Merged,
        _ => MergeMode::NonMerged,
    }
}

/// Collection completeness under `data_dir`, optionally for a single collection.
/// Factored out of the Tauri commands so it can be tested against a temporary
/// catalogue.
fn compute_status(
    data_dir: &Path,
    collection: Option<&str>,
    mode: MergeMode,
) -> anyhow::Result<Vec<CollectionStatus>> {
    let db = Database::open(&data_dir.join("db.sqlite"))?;
    ops::collection_status(db.conn(), collection, mode)
}

/// The catalogue's default data directory (`~/.cat198x`), shared with the CLI.
fn data_dir() -> anyhow::Result<std::path::PathBuf> {
    cat198x::cli::get_data_dir(None)
}

/// The saved plan trimmed for the UI: the summary and counts in full, the
/// operation list bounded to a reviewable prefix.
#[derive(serde::Serialize)]
struct PlanView {
    state_hash: String,
    created_at: String,
    total_operations: usize,
    summary: PlanSummary,
    operations: Vec<Operation>,
}

/// Every registered collection (name + whether it has an active version). Cheap
/// — the UI renders the list immediately, then fills each row's stats with
/// `status_one`, so it never blocks on the whole catalogue.
#[tauri::command]
async fn collections() -> Result<Vec<CollectionInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| -> anyhow::Result<_> {
        let db = Database::open(&data_dir()?.join("db.sqlite"))?;
        ops::list_collections(db.conn())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Completeness for a single collection by name. The UI calls this per row,
/// concurrently, so the heavy collections (e.g. MAME) fill in without holding up
/// the rest. Returns `null` if the collection isn't found.
///
/// `async` so Tauri runs it off the main thread; the blocking database work is
/// handed to `spawn_blocking`, so a slow collection can never freeze the UI.
#[tauri::command]
async fn status_one(
    name: String,
    merge_mode: Option<String>,
) -> Result<Option<CollectionStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<_> {
        let mode = parse_merge_mode(merge_mode.as_deref());
        Ok(compute_status(&data_dir()?, Some(&name), mode)?.into_iter().next())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// The most recent saved plan — the reorganisation as a diff — or `null`.
#[tauri::command]
async fn plan_diff() -> Result<Option<PlanView>, String> {
    tauri::async_runtime::spawn_blocking(|| -> anyhow::Result<Option<PlanView>> {
        let Some(plan) = ops::latest_plan(&data_dir()?)? else {
            return Ok(None);
        };
        let total_operations = plan.operations.len();
        let mut operations = plan.operations;
        operations.truncate(MAX_PLAN_OPS);
        Ok(Some(PlanView {
            state_hash: plan.state_hash,
            created_at: plan.created_at,
            total_operations,
            summary: plan.summary,
            operations,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![collections, status_one, plan_diff])
        .run(tauri::generate_context!())
        .expect("error while running the Cat198x UI");
}

#[cfg(test)]
mod tests {
    use super::*;
    use cat198x::db::{collections, dats};

    #[test]
    fn parse_merge_mode_defaults_to_non_merged() {
        assert_eq!(parse_merge_mode(Some("split")), MergeMode::Split);
        assert_eq!(parse_merge_mode(Some("merged")), MergeMode::Merged);
        assert_eq!(parse_merge_mode(None), MergeMode::NonMerged);
        assert_eq!(parse_merge_mode(Some("whatever")), MergeMode::NonMerged);
    }

    #[test]
    fn compute_status_reads_completeness_from_the_catalogue() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let db = Database::open(&tmp.path().join("db.sqlite")).unwrap();
            let conn = db.conn();
            let c = collections::create_collection(conn, "NES", "nointro").unwrap();
            let v = collections::add_version(conn, c, "v1", "/d/nes.dat", true).unwrap();
            let node = dats::create_node(conn, v, None, "NES", "dat", "NES").unwrap();
            let g = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
            dats::create_rom(conn, g, "a.nes", 10, Some("AAA"), None, None, "good", None).unwrap();
            dats::create_rom(conn, g, "b.nes", 10, Some("BBB"), None, None, "good", None).unwrap();
            conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
                .unwrap();
        }

        // All collections, and the per-collection path the UI uses, agree.
        let all = compute_status(tmp.path(), None, MergeMode::NonMerged).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "NES");
        assert_eq!(all[0].have_roms, 1);
        assert_eq!(all[0].missing_roms, 1);

        let one = compute_status(tmp.path(), Some("NES"), MergeMode::NonMerged).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "NES");
        assert_eq!(one[0].have_roms, 1);

        // An unknown collection yields nothing (the command maps this to null).
        assert!(
            compute_status(tmp.path(), Some("nope"), MergeMode::NonMerged)
                .unwrap()
                .is_empty()
        );
    }
}
